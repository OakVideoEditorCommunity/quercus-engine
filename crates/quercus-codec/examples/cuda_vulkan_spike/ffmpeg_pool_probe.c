// Quercus Video Editor - Non-Linear Video Editor
// Copyright (C) 2026 Quercus Team
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

// Route B integration probe: FFmpeg CUDA AVHWFramesContext with a
// caller-provided, exportable CUDA VMM frame pool, PLUS the Vulkan
// OPAQUE_FD import of the pool slot the frame actually lives in.
//
// Run on the GPU box (see run.sh):
//   run.sh builds it against the project FFmpeg prefix; the link line is
//   libavutil plus its Libs.private deps and -lvulkan (OPAQUE_FD import)
//
// Read-only with respect to the engine: it allocates its own GPU memory,
// hands it to FFmpeg as a frame pool, and prints what happens.

#define _GNU_SOURCE

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include <vulkan/vulkan.h>

#include <libavutil/buffer.h>
#include <libavutil/hwcontext.h>
#include <libavutil/imgutils.h>
#include <libavutil/pixfmt.h>

/* hwcontext_cuda.h needs the CUDA toolkit's <cuda.h>. The public layout of
 * AVCUDADeviceContext is three pointers; mirror it. */
typedef struct {
	void *cuda_ctx;
	void *stream;
	void *internal;
} ProbeAVCUDADeviceContext;

/* ------------------------------------------------------------------ */
/* CUDA driver API (dlopen)                                            */
/* ------------------------------------------------------------------ */

typedef int CUresult;
typedef void *CUcontext;
typedef uint64_t CUdeviceptr;
typedef uint64_t CUmemGenericAllocationHandle;
typedef int CUmemAllocationGranularity_flags;

#define CUDA_SUCCESS 0
#define CU_MEM_ALLOC_GRANULARITY_MINIMUM 0x0
#define CU_MEM_ALLOCATION_TYPE_PINNED 0x1
#define CU_MEM_LOCATION_TYPE_DEVICE 0x1
#define CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR 0x1
#define CU_MEM_ACCESS_FLAGS_PROT_READWRITE 0x3
#define CU_MEM_RANGE_HANDLE_TYPE_DMA_BUF_FD 0x1

struct cu_mem_alloc_prop {
	int32_t type;
	int32_t requested_handle_types;
	int32_t location_type;
	int32_t location_id;
	void *win32_handle_metadata;
	unsigned char compression_type;
	unsigned char gpu_direct_rdma_capable;
	unsigned short usage;
	unsigned char reserved[38];
};
struct cu_mem_access_desc {
	int32_t location_type;
	int32_t location_id;
	int32_t flags;
	int32_t pad;
};

typedef CUresult (*PFN_cuCtxPushCurrent)(CUcontext);
typedef CUresult (*PFN_cuCtxPopCurrent)(CUcontext *);
typedef CUresult (*PFN_cuCtxSetCurrent)(CUcontext);
typedef CUresult (*PFN_cuCtxGetDevice)(int *);
typedef CUresult (*PFN_cuMemGetAllocationGranularity)(
	size_t *, const struct cu_mem_alloc_prop *, CUmemAllocationGranularity_flags);
typedef CUresult (*PFN_cuMemCreate)(CUmemGenericAllocationHandle *, size_t,
                                    const struct cu_mem_alloc_prop *,
                                    unsigned long long);
typedef CUresult (*PFN_cuMemRelease)(CUmemGenericAllocationHandle);
typedef CUresult (*PFN_cuMemAddressReserve)(CUdeviceptr *, size_t, size_t,
                                            CUdeviceptr, unsigned long long);
typedef CUresult (*PFN_cuMemAddressFree)(CUdeviceptr, size_t);
typedef CUresult (*PFN_cuMemMap)(CUdeviceptr, size_t, size_t,
                                 CUmemGenericAllocationHandle,
                                 unsigned long long);
typedef CUresult (*PFN_cuMemUnmap)(CUdeviceptr, size_t);
typedef CUresult (*PFN_cuMemSetAccess)(CUdeviceptr, size_t,
                                       const struct cu_mem_access_desc *, size_t);
typedef CUresult (*PFN_cuMemExportToShareableHandle)(
	void *, CUmemGenericAllocationHandle, int, unsigned long long);
typedef CUresult (*PFN_cuMemGetHandleForAddressRange)(void *, CUdeviceptr, size_t,
                                                      int, unsigned long long);
typedef CUresult (*PFN_cuMemcpyHtoD)(CUdeviceptr, const void *, size_t);
typedef CUresult (*PFN_cuCtxSynchronize)(void);
typedef CUresult (*PFN_cuGetErrorName)(CUresult, const char **);
typedef CUresult (*PFN_cuGetErrorString)(CUresult, const char **);

static struct {
	void *dl;
	PFN_cuCtxPushCurrent push;
	PFN_cuCtxPopCurrent pop;
	PFN_cuCtxSetCurrent set_current;
	PFN_cuCtxGetDevice get_device;
	PFN_cuMemGetAllocationGranularity gran;
	PFN_cuMemCreate create;
	PFN_cuMemRelease release;
	PFN_cuMemAddressReserve reserve;
	PFN_cuMemAddressFree address_free;
	PFN_cuMemMap map;
	PFN_cuMemUnmap unmap;
	PFN_cuMemSetAccess access;
	PFN_cuMemExportToShareableHandle export;
	PFN_cuMemGetHandleForAddressRange range;
	PFN_cuMemcpyHtoD memcpy_htod;
	PFN_cuCtxSynchronize sync;
	PFN_cuGetErrorName err_name;
	PFN_cuGetErrorString err_string;
} cu;

static void *sym(const char *name, const char *alt)
{
	void *p = dlsym(cu.dl, name);
	if (!p && alt)
		p = dlsym(cu.dl, alt);
	if (!p)
		fprintf(stderr, "[cuda] missing %s\n", name);
	return p;
}

static int cuda_load(void)
{
	memset(&cu, 0, sizeof(cu));
	cu.dl = dlopen("libcuda.so.1", RTLD_NOW | RTLD_GLOBAL);
	if (!cu.dl)
		cu.dl = dlopen("libcuda.so", RTLD_NOW | RTLD_GLOBAL);
	if (!cu.dl) {
		fprintf(stderr, "[cuda] cannot load libcuda: %s\n", dlerror());
		return 0;
	}
	cu.push = (PFN_cuCtxPushCurrent)sym("cuCtxPushCurrent", "cuCtxPushCurrent_v2");
	cu.pop = (PFN_cuCtxPopCurrent)sym("cuCtxPopCurrent", "cuCtxPopCurrent_v2");
	cu.set_current = (PFN_cuCtxSetCurrent)sym("cuCtxSetCurrent", NULL);
	cu.get_device = (PFN_cuCtxGetDevice)sym("cuCtxGetDevice", NULL);
	cu.gran = (PFN_cuMemGetAllocationGranularity)sym(
		"cuMemGetAllocationGranularity", NULL);
	cu.create = (PFN_cuMemCreate)sym("cuMemCreate", NULL);
	cu.release = (PFN_cuMemRelease)sym("cuMemRelease", NULL);
	cu.reserve = (PFN_cuMemAddressReserve)sym("cuMemAddressReserve", NULL);
	cu.address_free = (PFN_cuMemAddressFree)sym("cuMemAddressFree", NULL);
	cu.map = (PFN_cuMemMap)sym("cuMemMap", NULL);
	cu.unmap = (PFN_cuMemUnmap)sym("cuMemUnmap", NULL);
	cu.access = (PFN_cuMemSetAccess)sym("cuMemSetAccess", NULL);
	cu.export = (PFN_cuMemExportToShareableHandle)sym(
		"cuMemExportToShareableHandle", NULL);
	cu.range = (PFN_cuMemGetHandleForAddressRange)sym(
		"cuMemGetHandleForAddressRange", NULL);
	cu.memcpy_htod = (PFN_cuMemcpyHtoD)sym("cuMemcpyHtoD", "cuMemcpyHtoD_v2");
	cu.sync = (PFN_cuCtxSynchronize)sym("cuCtxSynchronize", NULL);
	cu.err_name = (PFN_cuGetErrorName)sym("cuGetErrorName", NULL);
	cu.err_string = (PFN_cuGetErrorString)sym("cuGetErrorString", NULL);
	if (!cu.gran || !cu.create || !cu.reserve || !cu.map || !cu.access)
		return 0;
	return 1;
}

static const char *err(CUresult r)
{
	const char *n = NULL;
	if (cu.err_name && cu.err_name(r, &n) == CUDA_SUCCESS && n)
		return n;
	return "CUDA_ERROR(?)";
}
static const char *errs(CUresult r)
{
	const char *s = NULL;
	if (cu.err_string && cu.err_string(r, &s) == CUDA_SUCCESS && s)
		return s;
	return "";
}

/* The CUDA context FFmpeg created; re-assert it around every VMM operation
 * (FFmpeg pushes/pops its context around its own calls, so a context that
 * was current earlier can be non-current when our pool callback or our
 * pattern copy runs). */
static void *g_cuda_ctx;
static int g_device;

static int ctx_enter(const char *why)
{
	if (!g_cuda_ctx) {
		printf("[ctx] %s: no CUcontext\n", why);
		return 0;
	}
	CUresult p = cu.push ? cu.push(g_cuda_ctx) : CUDA_SUCCESS;
	CUresult s = cu.set_current ? cu.set_current(g_cuda_ctx) : CUDA_SUCCESS;
	int d = -1;
	CUresult g = cu.get_device ? cu.get_device(&d) : CUDA_SUCCESS;
	printf("[ctx] %s: push=%s set_current=%s get_device=%s device=%d\n", why,
	       err(p), err(s), err(g), d);
	if (d >= 0)
		g_device = d;
	return p == CUDA_SUCCESS;
}

static void ctx_leave(int pushed)
{
	if (pushed && cu.pop) {
		CUcontext dummy = NULL;
		cu.pop(&dummy);
	}
}

/* ------------------------------------------------------------------ */
/* Exportable VMM pool (all slots tracked, not just the first)         */
/* ------------------------------------------------------------------ */

#define MAX_SLOTS 64
struct pool_slot {
	CUmemGenericAllocationHandle handle;
	CUdeviceptr va;
	size_t size;
	int fd;      /* POSIX_FD export of the allocation, -1 if none */
	int range_fd;/* DMA_BUF range export of `va`, -1 if none */
};

static struct pool_slot *g_slots[MAX_SLOTS];
static int g_nslots;
static size_t g_granularity;

static void fd_kind(int fd, char *out, size_t n)
{
	char p[64], b[256];
	snprintf(p, sizeof(p), "/proc/self/fd/%d", fd);
	ssize_t r = readlink(p, b, sizeof(b) - 1);
	if (r < 0) {
		snprintf(out, n, "readlink: %s", strerror(errno));
		return;
	}
	b[r] = 0;
	snprintf(out, n, "%.*s", (int)(n - 1), b);
}

static void slot_free(void *opaque, uint8_t *data)
{
	struct pool_slot *s = opaque;
	(void)data;
	int pushed = ctx_enter("slot_free");
	printf("[pool] free slot va=0x%llx size=%zu\n", (unsigned long long)s->va,
	       s->size);
	if (s->fd >= 0)
		close(s->fd);
	if (s->range_fd >= 0)
		close(s->range_fd);
	if (cu.unmap)
		cu.unmap(s->va, s->size);
	if (cu.address_free)
		cu.address_free(s->va, s->size);
	if (cu.release)
		cu.release(s->handle);
	for (int i = 0; i < g_nslots; i++)
		if (g_slots[i] == s)
			g_slots[i] = NULL;
	ctx_leave(pushed);
	free(s);
}

static AVBufferRef *pool_alloc(void *opaque, size_t size)
{
	(void)opaque;
	int pushed = ctx_enter("pool_alloc");
	size_t gran = g_granularity ? g_granularity : 4096;
	size_t rounded = (size + gran - 1) / gran * gran;

	struct cu_mem_alloc_prop prop;
	memset(&prop, 0, sizeof(prop));
	prop.type = CU_MEM_ALLOCATION_TYPE_PINNED;
	prop.requested_handle_types = CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR;
	prop.location_type = CU_MEM_LOCATION_TYPE_DEVICE;
	prop.location_id = g_device;

	struct pool_slot *s = calloc(1, sizeof(*s));
	if (!s)
		return NULL;
	s->size = rounded;
	s->fd = -1;
	s->range_fd = -1;

	CUresult r = cu.create(&s->handle, rounded, &prop, 0);
	if (r != CUDA_SUCCESS) {
		printf("[pool] cuMemCreate(%zu, POSIX_FD) -> %s (%s)\n", rounded, err(r),
		       errs(r));
		prop.requested_handle_types = 0;
		r = cu.create(&s->handle, rounded, &prop, 0);
		printf("[pool] cuMemCreate(%zu, NONE) -> %s\n", rounded, err(r));
		if (r != CUDA_SUCCESS) {
			free(s);
			ctx_leave(pushed);
			return NULL;
		}
	}
	r = cu.reserve(&s->va, rounded, 0, 0, 0);
	if (r != CUDA_SUCCESS) {
		printf("[pool] cuMemAddressReserve -> %s\n", err(r));
		cu.release(s->handle);
		free(s);
		ctx_leave(pushed);
		return NULL;
	}
	r = cu.map(s->va, rounded, 0, s->handle, 0);
	if (r != CUDA_SUCCESS) {
		printf("[pool] cuMemMap -> %s\n", err(r));
		cu.address_free(s->va, rounded);
		cu.release(s->handle);
		free(s);
		ctx_leave(pushed);
		return NULL;
	}
	struct cu_mem_access_desc a;
	memset(&a, 0, sizeof(a));
	a.location_type = CU_MEM_LOCATION_TYPE_DEVICE;
	a.location_id = 0;
	a.flags = CU_MEM_ACCESS_FLAGS_PROT_READWRITE;
	a.location_id = g_device;
	r = cu.access(s->va, rounded, &a, 1);
	if (r != CUDA_SUCCESS)
		printf("[pool] cuMemSetAccess(device=%d) -> %s (%s)\n", g_device, err(r),
		       errs(r));

	int fd = -1;
	r = cu.export(&fd, s->handle, CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR, 0);
	if (r == CUDA_SUCCESS)
		s->fd = fd;
	else
		printf("[pool] cuMemExportToShareableHandle(POSIX_FD) -> %s\n", err(r));
	int fd2 = -1;
	if (cu.range) {
		r = cu.range(&fd2, s->va, rounded, CU_MEM_RANGE_HANDLE_TYPE_DMA_BUF_FD, 0);
		if (r == CUDA_SUCCESS)
			s->range_fd = fd2;
		else
			printf("[pool] cuMemGetHandleForAddressRange(VMM) -> %s\n", err(r));
	}

	if (g_nslots < MAX_SLOTS)
		g_slots[g_nslots] = s;
	int index = g_nslots++;
	printf("[pool] alloc #%d: requested=%zu rounded=%zu va=0x%llx fd=%d "
	       "range_fd=%d\n", index, size, rounded, (unsigned long long)s->va,
	       s->fd, s->range_fd);

	AVBufferRef *ref =
		av_buffer_create((uint8_t *)(uintptr_t)s->va, rounded, slot_free, s, 0);
	if (!ref) {
		slot_free(s, (uint8_t *)(uintptr_t)s->va);
		ctx_leave(pushed);
		return NULL;
	}
	ctx_leave(pushed);
	return ref;
}

/* ------------------------------------------------------------------ */
/* Minimal Vulkan device + OPAQUE_FD linear import                     */
/* ------------------------------------------------------------------ */

struct vk_min {
	VkInstance instance;
	VkPhysicalDevice phys;
	VkDevice dev;
	uint32_t qfam;
	VkQueue queue;
};

static uint32_t mem_type(VkPhysicalDevice phys, uint32_t bits,
                         VkMemoryPropertyFlags want)
{
	VkPhysicalDeviceMemoryProperties mp;
	vkGetPhysicalDeviceMemoryProperties(phys, &mp);
	for (uint32_t i = 0; i < mp.memoryTypeCount; i++)
		if ((bits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & want) == want)
			return i;
	return UINT32_MAX;
}

static int vk_min_init(struct vk_min *v)
{
	memset(v, 0, sizeof(*v));
	VkApplicationInfo app = { 0 };
	app.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO;
	app.pApplicationName = "quercus-route-b-pool-probe";
	app.apiVersion = VK_API_VERSION_1_3;
	VkInstanceCreateInfo ici = { 0 };
	ici.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO;
	ici.pApplicationInfo = &app;
	if (vkCreateInstance(&ici, NULL, &v->instance) != VK_SUCCESS)
		return 0;
	uint32_t n = 0;
	if (vkEnumeratePhysicalDevices(v->instance, &n, NULL) != VK_SUCCESS || n == 0)
		return 0;
	VkPhysicalDevice *devs = calloc(n, sizeof(*devs));
	vkEnumeratePhysicalDevices(v->instance, &n, devs);
	v->phys = devs[0];
	for (uint32_t i = 0; i < n; i++) {
		VkPhysicalDeviceProperties p;
		vkGetPhysicalDeviceProperties(devs[i], &p);
		if (p.vendorID == 0x10DE) {
			v->phys = devs[i];
			break;
		}
	}
	free(devs);
	VkPhysicalDeviceProperties p;
	vkGetPhysicalDeviceProperties(v->phys, &p);
	printf("[vk] device: %s (driver 0x%08x)\n", p.deviceName, p.driverVersion);

	uint32_t ec = 0;
	vkEnumerateDeviceExtensionProperties(v->phys, NULL, &ec, NULL);
	VkExtensionProperties *exts = calloc(ec ? ec : 1, sizeof(*exts));
	vkEnumerateDeviceExtensionProperties(v->phys, NULL, &ec, exts);
	const char *need[] = { VK_KHR_EXTERNAL_MEMORY_EXTENSION_NAME,
	                       VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME };
	uint32_t have = 0;
	for (size_t k = 0; k < 2; k++)
		for (uint32_t i = 0; i < ec; i++)
			if (strcmp(exts[i].extensionName, need[k]) == 0) {
				have++;
				break;
			}
	free(exts);
	if (have < 2) {
		printf("[vk] missing external-memory-fd extensions (%u/2)\n", have);
		return 0;
	}

	uint32_t qn = 0;
	vkGetPhysicalDeviceQueueFamilyProperties(v->phys, &qn, NULL);
	VkQueueFamilyProperties *q = calloc(qn ? qn : 1, sizeof(*q));
	vkGetPhysicalDeviceQueueFamilyProperties(v->phys, &qn, q);
	uint32_t qf = UINT32_MAX;
	for (uint32_t i = 0; i < qn; i++)
		if (q[i].queueFlags & (VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_TRANSFER_BIT)) {
			qf = i;
			break;
		}
	free(q);
	if (qf == UINT32_MAX)
		return 0;
	float prio = 1.0f;
	VkDeviceQueueCreateInfo qci = { 0 };
	qci.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO;
	qci.queueFamilyIndex = qf;
	qci.queueCount = 1;
	qci.pQueuePriorities = &prio;
	VkDeviceCreateInfo dci = { 0 };
	dci.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO;
	dci.queueCreateInfoCount = 1;
	dci.pQueueCreateInfos = &qci;
	dci.enabledExtensionCount = 2;
	dci.ppEnabledExtensionNames = need;
	if (vkCreateDevice(v->phys, &dci, NULL, &v->dev) != VK_SUCCESS)
		return 0;
	v->qfam = qf;
	vkGetDeviceQueue(v->dev, qf, 0, &v->queue);
	return 1;
}

/* Import `fd` as an R8 LINEAR external image, copy 16 bytes out, verify
 * that the first 8 bytes are the 0xAB pattern. */
static int vk_import_opaque(struct vk_min *v, int fd, uint32_t width,
                            uint32_t height, const char *label)
{
	if (fd < 0) {
		printf("  [%s] SKIP: no fd\n", label);
		return 0;
	}
	VkExternalMemoryImageCreateInfo ext = { 0 };
	ext.sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO;
	ext.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
	VkImageCreateInfo ici = { 0 };
	ici.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
	ici.imageType = VK_IMAGE_TYPE_2D;
	ici.format = VK_FORMAT_R8_UNORM;
	ici.extent.width = width;
	ici.extent.height = height;
	ici.extent.depth = 1;
	ici.mipLevels = 1;
	ici.arrayLayers = 1;
	ici.samples = VK_SAMPLE_COUNT_1_BIT;
	ici.tiling = VK_IMAGE_TILING_LINEAR;
	ici.usage = VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
	ici.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
	ici.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
	ici.pNext = &ext;
	VkImage image = VK_NULL_HANDLE;
	VkResult r = vkCreateImage(v->dev, &ici, NULL, &image);
	printf("  [%s] vkCreateImage -> %d\n", label, (int)r);
	if (r != VK_SUCCESS)
		return 0;

	VkMemoryRequirements req;
	vkGetImageMemoryRequirements(v->dev, image, &req);
	int dupfd = fcntl(fd, F_DUPFD_CLOEXEC, 3);
	VkMemoryDedicatedAllocateInfo ded = { 0 };
	ded.sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO;
	ded.image = image;
	VkImportMemoryFdInfoKHR imp = { 0 };
	imp.sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR;
	imp.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
	imp.fd = dupfd;
	imp.pNext = &ded;
	VkMemoryAllocateInfo mai = { 0 };
	mai.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
	mai.allocationSize = req.size;
	mai.memoryTypeIndex = mem_type(v->phys, req.memoryTypeBits,
	                               VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
	if (mai.memoryTypeIndex == UINT32_MAX)
		mai.memoryTypeIndex = mem_type(v->phys, req.memoryTypeBits, 0);
	mai.pNext = &imp;
	VkDeviceMemory mem = VK_NULL_HANDLE;
	r = vkAllocateMemory(v->dev, &mai, NULL, &mem);
	printf("  [%s] vkAllocateMemory(OPAQUE_FD import) -> %d\n", label, (int)r);
	if (r != VK_SUCCESS) {
		if (dupfd >= 0)
			close(dupfd);
		vkDestroyImage(v->dev, image, NULL);
		return 0;
	}
	r = vkBindImageMemory(v->dev, image, mem, 0);
	printf("  [%s] vkBindImageMemory -> %d\n", label, (int)r);

	VkBufferCreateInfo bci = { 0 };
	bci.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO;
	bci.size = 64;
	bci.usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT;
	VkBuffer buf = VK_NULL_HANDLE;
	vkCreateBuffer(v->dev, &bci, NULL, &buf);
	VkMemoryRequirements breq;
	vkGetBufferMemoryRequirements(v->dev, buf, &breq);
	VkMemoryAllocateInfo bmai = { 0 };
	bmai.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
	bmai.allocationSize = breq.size;
	bmai.memoryTypeIndex = mem_type(v->phys, breq.memoryTypeBits,
	                                VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
	                                VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
	VkDeviceMemory bmem = VK_NULL_HANDLE;
	vkAllocateMemory(v->dev, &bmai, NULL, &bmem);
	vkBindBufferMemory(v->dev, buf, bmem, 0);

	VkCommandPoolCreateInfo pci = { 0 };
	pci.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO;
	pci.queueFamilyIndex = v->qfam;
	VkCommandPool pool = VK_NULL_HANDLE;
	vkCreateCommandPool(v->dev, &pci, NULL, &pool);
	VkCommandBufferAllocateInfo cai = { 0 };
	cai.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO;
	cai.commandPool = pool;
	cai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
	cai.commandBufferCount = 1;
	VkCommandBuffer cmd;
	vkAllocateCommandBuffers(v->dev, &cai, &cmd);
	VkCommandBufferBeginInfo bi = { 0 };
	bi.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO;
	bi.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
	vkBeginCommandBuffer(cmd, &bi);
	VkImageMemoryBarrier b = { 0 };
	b.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER;
	b.oldLayout = VK_IMAGE_LAYOUT_UNDEFINED;
	b.newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
	b.srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
	b.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
	b.image = image;
	b.subresourceRange.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT;
	b.subresourceRange.levelCount = 1;
	b.subresourceRange.layerCount = 1;
	b.dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
	vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
	                     VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1,
	                     &b);
	VkBufferImageCopy region = { 0 };
	region.imageSubresource.aspectMask = VK_IMAGE_ASPECT_COLOR_BIT;
	region.imageSubresource.layerCount = 1;
	region.imageExtent.width = width < 16 ? width : 16;
	region.imageExtent.height = 1;
	region.imageExtent.depth = 1;
	vkCmdCopyImageToBuffer(cmd, image, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf,
	                       1, &region);
	vkEndCommandBuffer(cmd);
	VkSubmitInfo si = { 0 };
	si.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO;
	si.commandBufferCount = 1;
	si.pCommandBuffers = &cmd;
	VkResult sr = vkQueueSubmit(v->queue, 1, &si, VK_NULL_HANDLE);
	if (sr == VK_SUCCESS)
		sr = vkQueueWaitIdle(v->queue);
	printf("  [%s] image->buffer copy -> %d\n", label, (int)sr);
	int ok = 0;
	if (sr == VK_SUCCESS) {
		void *mapped = NULL;
		if (vkMapMemory(v->dev, bmem, 0, 16, 0, &mapped) == VK_SUCCESS) {
			unsigned char *p = mapped;
			printf("        first bytes:");
			for (int i = 0; i < 8; i++)
				printf(" %02x", p[i]);
			printf("  -> %s\n", p[0] == 0xAB ? "MATCH (0xab)" : "MISMATCH");
			ok = p[0] == 0xAB;
			vkUnmapMemory(v->dev, bmem);
		}
	}
	vkDestroyCommandPool(v->dev, pool, NULL);
	vkDestroyBuffer(v->dev, buf, NULL);
	vkFreeMemory(v->dev, bmem, NULL);
	vkFreeMemory(v->dev, mem, NULL);
	vkDestroyImage(v->dev, image, NULL);
	return ok;
}

/* ------------------------------------------------------------------ */
/* main                                                                */
/* ------------------------------------------------------------------ */

static struct pool_slot *slot_for(uint64_t dptr)
{
	for (int i = 0; i < g_nslots; i++) {
		struct pool_slot *s = g_slots[i];
		if (s && dptr >= s->va && dptr < s->va + s->size)
			return s;
	}
	return NULL;
}

static int slot_index(struct pool_slot *s)
{
	for (int i = 0; i < g_nslots; i++)
		if (g_slots[i] == s)
			return i;
	return -1;
}

int main(void)
{
	printf("Quercus route B probe: FFmpeg CUDA frames + exportable VMM pool\n");
	printf("libavutil: %u.%u.%u\n", AV_VERSION_MAJOR(avutil_version()),
	       AV_VERSION_MINOR(avutil_version()), AV_VERSION_MICRO(avutil_version()));

	if (!cuda_load()) {
		printf("[fatal] CUDA driver API unavailable\n");
		return 1;
	}

	AVBufferRef *dev_ref = NULL;
	int ret = av_hwdevice_ctx_create(&dev_ref, AV_HWDEVICE_TYPE_CUDA, NULL, NULL, 0);
	printf("[ffmpeg] av_hwdevice_ctx_create(CUDA) -> %d\n", ret);
	if (ret < 0 || !dev_ref) {
		printf("[fatal] no CUDA device (run on the GPU box)\n");
		return 1;
	}
	AVHWDeviceContext *dev = (AVHWDeviceContext *)dev_ref->data;
	ProbeAVCUDADeviceContext *cudactx = dev->hwctx;
	g_cuda_ctx = cudactx->cuda_ctx;
	printf("[ffmpeg] CUcontext=%p\n", cudactx->cuda_ctx);
	int init_pushed = ctx_enter("hwdevice created");
	ctx_leave(init_pushed);

	const int W = 1920, H = 1080;
	size_t need = (size_t)av_image_get_buffer_size(AV_PIX_FMT_P010, W, H, 4096);
	struct cu_mem_alloc_prop prop;
	memset(&prop, 0, sizeof(prop));
	prop.type = CU_MEM_ALLOCATION_TYPE_PINNED;
	prop.requested_handle_types = CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR;
	prop.location_type = CU_MEM_LOCATION_TYPE_DEVICE;
	prop.location_id = 0;
	CUresult r = cu.gran(&g_granularity, &prop, CU_MEM_ALLOC_GRANULARITY_MINIMUM);
	printf("[vmm] granularity -> %s, %zu bytes\n", err(r), g_granularity);
	if (r != CUDA_SUCCESS)
		g_granularity = 4096;
	printf("[pool] P010 %dx%d needs %zu bytes (align 4096)\n", W, H, need);

	AVBufferRef *frames_ref = av_hwframe_ctx_alloc(dev_ref);
	if (!frames_ref) {
		printf("[fatal] av_hwframe_ctx_alloc failed\n");
		return 1;
	}
	AVHWFramesContext *frames = (AVHWFramesContext *)frames_ref->data;
	frames->format = AV_PIX_FMT_CUDA;
	frames->sw_format = AV_PIX_FMT_P010;
	frames->width = W;
	frames->height = H;
	frames->initial_pool_size = 2;
	frames->pool = av_buffer_pool_init2(need, NULL, pool_alloc, NULL);
	printf("[ffmpeg] frames->pool = %p (caller-supplied)\n", (void *)frames->pool);

	ret = av_hwframe_ctx_init(frames_ref);
	printf("[ffmpeg] av_hwframe_ctx_init -> %d\n", ret);
	if (ret < 0) {
		printf("[fatal] frames init failed (pool rejected?)\n");
		return 1;
	}

	/* Keep BOTH frames alive: AVBufferPool hands back the last returned
	 * buffer first (LIFO), so freeing frame 0 before requesting frame 1
	 * makes both land in the same slot. */
	AVFrame *f[2] = { av_frame_alloc(), av_frame_alloc() };
	for (int i = 0; i < 2; i++) {
		ret = av_hwframe_get_buffer(frames_ref, f[i], 0);
		uint64_t d0 = ret == 0 ? (uint64_t)(uintptr_t)f[i]->data[0] : 0;
		struct pool_slot *s = ret == 0 ? slot_for(d0) : NULL;
		printf("[ffmpeg] get_buffer #%d -> %d, data[0]=0x%llx linesize[0]=%d "
		       "linesize[1]=%d buf0_size=%zu\n",
		       i, ret, (unsigned long long)d0, f[i]->linesize[0],
		       f[i]->linesize[1], f[i]->buf[0] ? (size_t)f[i]->buf[0]->size : 0);
		if (ret == 0) {
			printf("        inside caller VMM slot: %s", s ? "YES" : "NO");
			if (s)
				printf(" (slot #%d, offset=0x%llx, va=0x%llx)",
				       slot_index(s),
				       (unsigned long long)(d0 - s->va),
				       (unsigned long long)s->va);
			printf("\n");
		}
	}

	struct pool_slot *matched =
		f[0] && f[0]->data[0] ? slot_for((uint64_t)(uintptr_t)f[0]->data[0])
		                      : NULL;
	if (matched && cu.memcpy_htod) {
		unsigned char pattern[256];
		memset(pattern, 0xAB, sizeof(pattern));
		int pushed = ctx_enter("pattern copy");
		CUresult wr = cu.memcpy_htod(matched->va, pattern, sizeof(pattern));
		CUresult sy = cu.sync ? cu.sync() : CUDA_SUCCESS;
		printf("[slot] cuMemcpyHtoD(va=0x%llx, 0xAB, 256) -> %s (%s), "
		       "sync -> %s\n", (unsigned long long)matched->va, err(wr),
		       errs(wr), err(sy));
		ctx_leave(pushed);
	}

	if (matched) {
		char k1[256] = "(none)";
		if (matched->fd >= 0)
			fd_kind(matched->fd, k1, sizeof(k1));
		printf("\n[result] matched slot #%d: va=0x%llx size=%zu page_aligned=%s\n",
		       slot_index(matched), (unsigned long long)matched->va,
		       matched->size, (matched->va % 4096) == 0 ? "YES" : "NO");
		printf("[result] POSIX_FD export fd=%d kind=%s\n", matched->fd, k1);
		printf("[result] DMA_BUF range fd=%d\n", matched->range_fd);

		struct vk_min vk;
		if (vk_min_init(&vk)) {
			printf("\n[routeB] Vulkan import of the FFmpeg pool slot fd "
			       "(expect 0xab):\n");
			int ok = vk_import_opaque(&vk, matched->fd, W, H,
			                          "FFmpeg slot -> OPAQUE_FD + LINEAR");
			printf("[routeB] FFmpeg-slot fd import: %s\n",
			       ok ? "GO (data verified)" : "FAIL");
			if (vk.dev)
				vkDestroyDevice(vk.dev, NULL);
			if (vk.instance)
				vkDestroyInstance(vk.instance, NULL);
		} else {
			printf("[routeB] Vulkan device unavailable; imported fd test "
			       "skipped\n");
		}
	} else {
		printf("\n[result] FAIL: no frame landed in a caller VMM slot\n");
	}

	for (int i = 0; i < 2; i++)
		if (f[i])
			av_frame_free(&f[i]);
	av_buffer_unref(&frames_ref);
	av_buffer_unref(&dev_ref);
	if (cu.pop) {
		CUcontext dummy = NULL;
		cu.pop(&dummy);
	}
	return 0;
}
