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

//! Standalone spike probe: NVIDIA Linux CUDA -> Vulkan zero-copy import.
//!
//! This program answers, on the *target* machine (the one with a working
//! NVIDIA device node), the questions that cannot be answered by reading
//! code:
//!
//!   1. Does the CUDA driver export a page-locked `cuMemAlloc` range as a
//!      DMA-BUF fd (`cuMemGetHandleForAddressRange`, handle type
//!      `CU_MEM_RANGE_HANDLE_TYPE_DMA_BUF_FD`), and are the FFmpeg CUDA
//!      frame pointers (base address, size) usable for it?
//!   2. Can the resulting fd be imported into Vulkan as a linear image and
//!      read back (full GPU path: CUDA memory -> dma-buf -> VkImage ->
//!      VkBuffer -> host)?
//!   3. What do the Vulkan Video decode/encode capabilities say about
//!      H.264/HEVC 10-bit and 4:2:2 on this GPU+driver?
//!
//! Build/run (no CUDA toolkit required, no cargo, does not touch the
//! engine build):
//!
//!     cc -O2 -o cuda_vulkan_probe cuda_vulkan_probe.c -ldl -lvulkan
//!     ./cuda_vulkan_probe
//!
//! The probe is read-only with respect to the engine: it allocates its own
//! CUDA memory, exports it, imports it, copies it back and prints results.
//! It never writes project data and never loads FFmpeg.

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

#include <sys/socket.h>
#include <sys/wait.h>

#include <vulkan/vulkan.h>

/* ------------------------------------------------------------------ */
/* CUDA driver API (dlopen only: no cuda.h / toolkit needed)           */
/* ------------------------------------------------------------------ */

typedef int CUresult;
typedef int CUdevice;
typedef void *CUcontext;
typedef void *CUstream;
typedef uint64_t CUdeviceptr;
typedef int CUmemRangeHandleType;

#define CUDA_SUCCESS 0

#define CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT 14
#define CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED 124
#define CU_DEVICE_ATTRIBUTE_HOST_ALLOC_DMA_BUF_SUPPORTED 146
#define CU_DEVICE_ATTRIBUTE_DMA_BUF_MMAP_SUPPORTED 152

#define CU_MEM_RANGE_HANDLE_TYPE_DMA_BUF_FD 0x1
#define CU_MEM_RANGE_FLAG_DMA_BUF_MAPPING_TYPE_PCIE 0x1ULL

typedef CUresult (*PFN_cuInit)(unsigned int);
typedef CUresult (*PFN_cuDeviceGet)(CUdevice *, int);
typedef CUresult (*PFN_cuDeviceGetAttribute)(int *, int, CUdevice);
typedef CUresult (*PFN_cuDeviceGetName)(char *, int, CUdevice);
typedef CUresult (*PFN_cuCtxCreate)(CUcontext *, unsigned int, CUdevice);
typedef CUresult (*PFN_cuCtxDestroy)(CUcontext);
typedef CUresult (*PFN_cuCtxPushCurrent)(CUcontext);
typedef CUresult (*PFN_cuCtxPopCurrent)(CUcontext *);
typedef CUresult (*PFN_cuMemAlloc)(CUdeviceptr *, size_t);
typedef CUresult (*PFN_cuMemFree)(CUdeviceptr);
typedef CUresult (*PFN_cuMemGetHandleForAddressRange)(void *, CUdeviceptr,
                                                      size_t, CUmemRangeHandleType,
                                                      unsigned long long);
typedef CUresult (*PFN_cuGetErrorName)(CUresult, const char **);
typedef CUresult (*PFN_cuGetErrorString)(CUresult, const char **);

/* --- virtual memory management (route B) --- */
typedef uint64_t CUmemGenericAllocationHandle;
typedef int CUmemAllocationGranularity_flags;
#define CU_MEM_ALLOC_GRANULARITY_MINIMUM 0x0
#define CU_MEM_ALLOC_GRANULARITY_RECOMMENDED 0x1
#define CU_MEM_ALLOCATION_TYPE_PINNED 0x1
#define CU_MEM_LOCATION_TYPE_DEVICE 0x1
#define CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR 0x1
#define CU_MEM_ACCESS_FLAGS_PROT_READWRITE 0x3
#define CU_DEVICE_ATTRIBUTE_VIRTUAL_MEMORY_MANAGEMENT_SUPPORTED 102
#define CU_DEVICE_ATTRIBUTE_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR_SUPPORTED 103

/* CUDA 13 layout: type, requestedHandleTypes, location{type,id},
 * win32HandleMetaData, then two bytes of allocFlags. Declared with slack
 * (zero-filled); CUDA reads only its own fields. */
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
typedef CUresult (*PFN_cuMemsetD8)(CUdeviceptr, unsigned char, size_t);
typedef CUresult (*PFN_cuCtxSetCurrent)(CUcontext);
typedef CUresult (*PFN_cuCtxGetDevice)(int *);
typedef CUresult (*PFN_cuMemcpyHtoD)(CUdeviceptr, const void *, size_t);
typedef CUresult (*PFN_cuCtxSynchronize)(void);
typedef CUresult (*PFN_cuMemGetAllocationPropertiesFromHandle)(
	struct cu_mem_alloc_prop *, CUmemGenericAllocationHandle);

struct cuda_api {
	void *dl;
	PFN_cuInit init;
	PFN_cuDeviceGet device_get;
	PFN_cuDeviceGetAttribute device_attr;
	PFN_cuDeviceGetName device_name;
	PFN_cuCtxCreate ctx_create;
	PFN_cuCtxDestroy ctx_destroy;
	PFN_cuCtxPushCurrent ctx_push;
	PFN_cuCtxPopCurrent ctx_pop;
	PFN_cuMemAlloc mem_alloc;
	PFN_cuMemFree mem_free;
	PFN_cuMemGetHandleForAddressRange mem_handle_range;
	PFN_cuGetErrorName err_name;
	PFN_cuGetErrorString err_string;
	/* route B */
	PFN_cuMemGetAllocationGranularity granularity;
	PFN_cuMemCreate mem_create;
	PFN_cuMemRelease mem_release;
	PFN_cuMemAddressReserve addr_reserve;
	PFN_cuMemAddressFree addr_free;
	PFN_cuMemMap mem_map;
	PFN_cuMemUnmap mem_unmap;
	PFN_cuMemSetAccess mem_set_access;
	PFN_cuMemExportToShareableHandle mem_export;
	PFN_cuMemsetD8 memset_d8;
	PFN_cuCtxSetCurrent ctx_set_current;
	PFN_cuCtxGetDevice ctx_get_device;
	PFN_cuMemcpyHtoD memcpy_htod;
	PFN_cuCtxSynchronize ctx_sync;
	PFN_cuMemGetAllocationPropertiesFromHandle alloc_props;
};

static void *cuda_sym(struct cuda_api *cu, const char *name, const char *v2name)
{
	void *p = dlsym(cu->dl, name);
	if (!p && v2name)
		p = dlsym(cu->dl, v2name);
	if (!p)
		fprintf(stderr, "[cuda] missing symbol %s\n", name);
	return p;
}

static int cuda_load(struct cuda_api *cu)
{
	memset(cu, 0, sizeof(*cu));
	/* The loader name is stable across driver packages; libcuda.so.1 is
	 * the runtime, libcuda.so the (often absent) dev symlink. */
	const char *candidates[] = { "libcuda.so.1", "libcuda.so" };
	for (size_t i = 0; i < sizeof(candidates) / sizeof(candidates[0]); i++) {
		cu->dl = dlopen(candidates[i], RTLD_NOW | RTLD_GLOBAL);
		if (cu->dl)
			break;
	}
	if (!cu->dl) {
		fprintf(stderr, "[cuda] cannot dlopen libcuda.so.1: %s\n", dlerror());
		return 0;
	}
	cu->init = (PFN_cuInit)cuda_sym(cu, "cuInit", NULL);
	cu->device_get = (PFN_cuDeviceGet)cuda_sym(cu, "cuDeviceGet", NULL);
	cu->device_attr =
		(PFN_cuDeviceGetAttribute)cuda_sym(cu, "cuDeviceGetAttribute", NULL);
	cu->device_name = (PFN_cuDeviceGetName)cuda_sym(cu, "cuDeviceGetName", NULL);
	cu->ctx_create = (PFN_cuCtxCreate)cuda_sym(cu, "cuCtxCreate_v2", "cuCtxCreate");
	cu->ctx_destroy =
		(PFN_cuCtxDestroy)cuda_sym(cu, "cuCtxDestroy", "cuCtxDestroy_v2");
	cu->ctx_push = (PFN_cuCtxPushCurrent)cuda_sym(cu, "cuCtxPushCurrent_v2",
	                                              "cuCtxPushCurrent");
	cu->ctx_pop = (PFN_cuCtxPopCurrent)cuda_sym(cu, "cuCtxPopCurrent_v2",
	                                            "cuCtxPopCurrent");
	cu->mem_alloc = (PFN_cuMemAlloc)cuda_sym(cu, "cuMemAlloc_v2", "cuMemAlloc");
	cu->mem_free = (PFN_cuMemFree)cuda_sym(cu, "cuMemFree_v2", "cuMemFree");
	cu->mem_handle_range = (PFN_cuMemGetHandleForAddressRange)cuda_sym(
		cu, "cuMemGetHandleForAddressRange", NULL);
	cu->err_name = (PFN_cuGetErrorName)cuda_sym(cu, "cuGetErrorName", NULL);
	cu->err_string =
		(PFN_cuGetErrorString)cuda_sym(cu, "cuGetErrorString", NULL);
	cu->granularity = (PFN_cuMemGetAllocationGranularity)cuda_sym(
		cu, "cuMemGetAllocationGranularity", NULL);
	cu->mem_create =
		(PFN_cuMemCreate)cuda_sym(cu, "cuMemCreate", NULL);
	cu->mem_release =
		(PFN_cuMemRelease)cuda_sym(cu, "cuMemRelease", NULL);
	cu->addr_reserve =
		(PFN_cuMemAddressReserve)cuda_sym(cu, "cuMemAddressReserve", NULL);
	cu->addr_free =
		(PFN_cuMemAddressFree)cuda_sym(cu, "cuMemAddressFree", NULL);
	cu->mem_map = (PFN_cuMemMap)cuda_sym(cu, "cuMemMap", NULL);
	cu->mem_unmap = (PFN_cuMemUnmap)cuda_sym(cu, "cuMemUnmap", NULL);
	cu->mem_set_access =
		(PFN_cuMemSetAccess)cuda_sym(cu, "cuMemSetAccess", NULL);
	cu->mem_export = (PFN_cuMemExportToShareableHandle)cuda_sym(
		cu, "cuMemExportToShareableHandle", NULL);
	cu->memset_d8 =
		(PFN_cuMemsetD8)cuda_sym(cu, "cuMemsetD8_v2", "cuMemsetD8");
	cu->ctx_set_current =
		(PFN_cuCtxSetCurrent)cuda_sym(cu, "cuCtxSetCurrent", NULL);
	cu->ctx_get_device =
		(PFN_cuCtxGetDevice)cuda_sym(cu, "cuCtxGetDevice", NULL);
	cu->memcpy_htod =
		(PFN_cuMemcpyHtoD)cuda_sym(cu, "cuMemcpyHtoD_v2", "cuMemcpyHtoD");
	cu->ctx_sync = (PFN_cuCtxSynchronize)cuda_sym(cu, "cuCtxSynchronize", NULL);
	cu->alloc_props = (PFN_cuMemGetAllocationPropertiesFromHandle)cuda_sym(
		cu, "cuMemGetAllocationPropertiesFromHandle", NULL);

	if (!cu->init || !cu->device_get || !cu->device_attr || !cu->ctx_create ||
	    !cu->mem_alloc || !cu->mem_free || !cu->mem_handle_range)
		return 0;
	return 1;
}

static const char *cuda_err_name(struct cuda_api *cu, CUresult r)
{
	static char buf[128];
	const char *name = NULL;
	if (cu->err_name && cu->err_name(r, &name) == CUDA_SUCCESS && name)
		return name;
	snprintf(buf, sizeof(buf), "CUDA_ERROR(%d)", r);
	return buf;
}

static const char *cuda_err_string(struct cuda_api *cu, CUresult r)
{
	const char *s = NULL;
	if (cu->err_string && cu->err_string(r, &s) == CUDA_SUCCESS && s)
		return s;
	return "";
}

/* ------------------------------------------------------------------ */
/* Small helpers                                                       */
/* ------------------------------------------------------------------ */

static const char *vkres_str(VkResult r)
{
	switch (r) {
	case VK_SUCCESS: return "VK_SUCCESS";
	case VK_INCOMPLETE: return "VK_INCOMPLETE";
	case VK_ERROR_OUT_OF_HOST_MEMORY: return "VK_ERROR_OUT_OF_HOST_MEMORY";
	case VK_ERROR_OUT_OF_DEVICE_MEMORY: return "VK_ERROR_OUT_OF_DEVICE_MEMORY";
	case VK_ERROR_INITIALIZATION_FAILED: return "VK_ERROR_INITIALIZATION_FAILED";
	case VK_ERROR_EXTENSION_NOT_PRESENT: return "VK_ERROR_EXTENSION_NOT_PRESENT";
	case VK_ERROR_FORMAT_NOT_SUPPORTED: return "VK_ERROR_FORMAT_NOT_SUPPORTED";
	case VK_ERROR_VIDEO_PROFILE_OPERATION_NOT_SUPPORTED_KHR:
		return "VK_ERROR_VIDEO_PROFILE_OPERATION_NOT_SUPPORTED_KHR";
	case VK_ERROR_VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR:
		return "VK_ERROR_VIDEO_PROFILE_FORMAT_NOT_SUPPORTED_KHR";
	case VK_ERROR_VIDEO_PICTURE_LAYOUT_NOT_SUPPORTED_KHR:
		return "VK_ERROR_VIDEO_PICTURE_LAYOUT_NOT_SUPPORTED_KHR";
	case VK_ERROR_IMAGE_USAGE_NOT_SUPPORTED_KHR:
		return "VK_ERROR_IMAGE_USAGE_NOT_SUPPORTED_KHR";
	default: {
		static char buf[64];
		snprintf(buf, sizeof(buf), "VkResult(%d)", (int)r);
		return buf;
	}
	}
}

/* ------------------------------------------------------------------ */
/* Vulkan device selection                                             */
/* ------------------------------------------------------------------ */

struct vk_state {
	VkInstance instance;
	VkPhysicalDevice phys;
	VkPhysicalDeviceProperties props;
	VkDevice dev;
	uint32_t queue_family;
	VkQueue queue;
	uint32_t api_version;
	/* enabled device extensions (subset we care about) */
	int has_ext_dma_buf;
	int has_ext_drm_modifier;
	int has_ext_mem_fd;
	int has_ext_mem;
	int has_video_queue;
	int has_video_decode_h264;
	int has_video_decode_h265;
	int has_video_encode_h264;
	int has_video_encode_h265;

	PFN_vkGetPhysicalDeviceVideoCapabilitiesKHR get_video_caps;
	PFN_vkGetPhysicalDeviceVideoFormatPropertiesKHR get_video_formats;
};

static int vk_ext_supported(VkPhysicalDevice phys, const char *name)
{
	uint32_t n = 0;
	if (vkEnumerateDeviceExtensionProperties(phys, NULL, &n, NULL) != VK_SUCCESS)
		return 0;
	VkExtensionProperties *arr = calloc(n, sizeof(*arr));
	if (!arr)
		return 0;
	int found = 0;
	if (vkEnumerateDeviceExtensionProperties(phys, NULL, &n, arr) == VK_SUCCESS) {
		for (uint32_t i = 0; i < n; i++) {
			if (strcmp(arr[i].extensionName, name) == 0) {
				found = 1;
				break;
			}
		}
	}
	free(arr);
	return found;
}

static int vk_init(struct vk_state *s)
{
	memset(s, 0, sizeof(*s));

	/* api 1.1 gives vkGetPhysicalDeviceProperties2 in core (the image
	 * format query the import path needs). Video is queried through
	 * instance-level function pointers regardless. */
	VkApplicationInfo app = { 0 };
	app.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO;
	app.pApplicationName = "quercus-cuda-vulkan-probe";
	app.apiVersion = VK_API_VERSION_1_3;

	VkInstanceCreateInfo ici = { 0 };
	ici.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO;
	ici.pApplicationInfo = &app;

	VkResult r = vkCreateInstance(&ici, NULL, &s->instance);
	if (r != VK_SUCCESS) {
		printf("[vulkan] vkCreateInstance failed: %s\n", vkres_str(r));
		return 0;
	}

	uint32_t count = 0;
	r = vkEnumeratePhysicalDevices(s->instance, &count, NULL);
	if (r != VK_SUCCESS || count == 0) {
		printf("[vulkan] no physical devices (count=%u, %s)\n", count,
		       vkres_str(r));
		return 0;
	}
	VkPhysicalDevice *devs = calloc(count, sizeof(*devs));
	vkEnumeratePhysicalDevices(s->instance, &count, devs);
	printf("[vulkan] %u physical device(s):\n", count);
	for (uint32_t i = 0; i < count; i++) {
		VkPhysicalDeviceProperties p;
		vkGetPhysicalDeviceProperties(devs[i], &p);
		printf("  [%u] %s (vendor 0x%04x, type %d, api %u.%u.%u, driver 0x%08x)\n",
		       i, p.deviceName, p.vendorID, (int)p.deviceType,
		       VK_VERSION_MAJOR(p.apiVersion), VK_VERSION_MINOR(p.apiVersion),
		       VK_VERSION_PATCH(p.apiVersion), p.driverVersion);
	}
	/* Prefer an NVIDIA discrete GPU. */
	s->phys = devs[0];
	for (uint32_t i = 0; i < count; i++) {
		VkPhysicalDeviceProperties p;
		vkGetPhysicalDeviceProperties(devs[i], &p);
		if (p.vendorID == 0x10DE && p.deviceType == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU) {
			s->phys = devs[i];
			break;
		}
	}
	free(devs);
	vkGetPhysicalDeviceProperties(s->phys, &s->props);
	s->api_version = s->props.apiVersion;
	printf("[vulkan] using: %s\n", s->props.deviceName);

	s->has_ext_mem = vk_ext_supported(s->phys, VK_KHR_EXTERNAL_MEMORY_EXTENSION_NAME);
	s->has_ext_mem_fd =
		vk_ext_supported(s->phys, VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME);
	s->has_ext_dma_buf =
		vk_ext_supported(s->phys, "VK_EXT_external_memory_dma_buf");
	s->has_ext_drm_modifier =
		vk_ext_supported(s->phys, "VK_EXT_image_drm_format_modifier");
	s->has_video_queue = vk_ext_supported(s->phys, "VK_KHR_video_queue");
	s->has_video_decode_h264 =
		vk_ext_supported(s->phys, "VK_KHR_video_decode_h264");
	s->has_video_decode_h265 =
		vk_ext_supported(s->phys, "VK_KHR_video_decode_h265");
	s->has_video_encode_h264 =
		vk_ext_supported(s->phys, "VK_KHR_video_encode_h264");
	s->has_video_encode_h265 =
		vk_ext_supported(s->phys, "VK_KHR_video_encode_h265");

	printf("[vulkan] extensions: dma_buf=%d drm_modifier=%d mem_fd=%d "
	       "video_queue=%d vdec_h264=%d vdec_h265=%d venc_h264=%d venc_h265=%d\n",
	       s->has_ext_dma_buf, s->has_ext_drm_modifier, s->has_ext_mem_fd,
	       s->has_video_queue, s->has_video_decode_h264,
	       s->has_video_decode_h265, s->has_video_encode_h264,
	       s->has_video_encode_h265);

	s->get_video_caps = (PFN_vkGetPhysicalDeviceVideoCapabilitiesKHR)
		vkGetInstanceProcAddr(s->instance, "vkGetPhysicalDeviceVideoCapabilitiesKHR");
	s->get_video_formats = (PFN_vkGetPhysicalDeviceVideoFormatPropertiesKHR)
		vkGetInstanceProcAddr(s->instance,
		                      "vkGetPhysicalDeviceVideoFormatPropertiesKHR");

	/* --- device: enable the external-memory extensions we need ----- */
	const char *wanted[16];
	uint32_t nw = 0;
	if (s->has_ext_mem) wanted[nw++] = VK_KHR_EXTERNAL_MEMORY_EXTENSION_NAME;
	if (s->has_ext_mem_fd) wanted[nw++] = VK_KHR_EXTERNAL_MEMORY_FD_EXTENSION_NAME;
	if (s->has_ext_dma_buf) wanted[nw++] = "VK_EXT_external_memory_dma_buf";
	if (s->has_ext_drm_modifier) wanted[nw++] = "VK_EXT_image_drm_format_modifier";

	VkDeviceCreateInfo dci = { 0 };
	dci.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO;
	dci.enabledExtensionCount = nw;
	dci.ppEnabledExtensionNames = wanted;

	/* queue: first family with graphics or transfer support */
	uint32_t qn = 0;
	vkGetPhysicalDeviceQueueFamilyProperties(s->phys, &qn, NULL);
	VkQueueFamilyProperties *qprops = calloc(qn ? qn : 1, sizeof(*qprops));
	vkGetPhysicalDeviceQueueFamilyProperties(s->phys, &qn, qprops);
	uint32_t qfam = UINT32_MAX;
	for (uint32_t i = 0; i < qn; i++) {
		if (qprops[i].queueFlags & (VK_QUEUE_GRAPHICS_BIT | VK_QUEUE_TRANSFER_BIT)) {
			qfam = i;
			break;
		}
	}
	free(qprops);
	if (qfam == UINT32_MAX && qn > 0)
		qfam = 0;
	if (qfam == UINT32_MAX) {
		printf("[vulkan] no queue families; cannot create device\n");
		return 0;
	}
	float prio = 1.0f;
	VkDeviceQueueCreateInfo qci = { 0 };
	qci.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO;
	qci.queueFamilyIndex = qfam;
	qci.queueCount = 1;
	qci.pQueuePriorities = &prio;
	dci.queueCreateInfoCount = 1;
	dci.pQueueCreateInfos = &qci;
	/* the external-memory paths need the "external memory" feature bits
	 * per format, not global features; nothing extra to enable here. */
	r = vkCreateDevice(s->phys, &dci, NULL, &s->dev);
	if (r != VK_SUCCESS) {
		printf("[vulkan] vkCreateDevice(enabled ext=%u) failed: %s\n", nw,
		       vkres_str(r));
		/* retry without extensions so the capability queries above stay
		 * useful */
		dci.enabledExtensionCount = 0;
		r = vkCreateDevice(s->phys, &dci, NULL, &s->dev);
		if (r != VK_SUCCESS) {
			printf("[vulkan] vkCreateDevice(minimal) failed: %s\n",
			       vkres_str(r));
			return 0;
		}
	}
	s->queue_family = qfam;
	vkGetDeviceQueue(s->dev, qfam, 0, &s->queue);
	return 1;
}

/* ------------------------------------------------------------------ */
/* Vulkan Video capabilities                                           */
/* ------------------------------------------------------------------ */

static void video_caps_and_formats(struct vk_state *s, const char *label,
                                   VkVideoProfileInfoKHR *profile,
                                   void *codec_caps_struct,
                                   VkStructureType codec_caps_type,
                                   VkImageUsageFlags usage)
{
	if (!s->get_video_caps || !s->get_video_formats) {
		printf("  [%s] video entry points unavailable\n", label);
		return;
	}
	VkVideoCapabilitiesKHR caps = { 0 };
	caps.sType = VK_STRUCTURE_TYPE_VIDEO_CAPABILITIES_KHR;
	VkVideoDecodeCapabilitiesKHR dec = { 0 };
	VkVideoEncodeCapabilitiesKHR enc = { 0 };
	if (codec_caps_struct) {
		if (codec_caps_type == VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_CAPABILITIES_KHR ||
		    codec_caps_type == VK_STRUCTURE_TYPE_VIDEO_DECODE_H265_CAPABILITIES_KHR) {
			dec.sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_CAPABILITIES_KHR;
			dec.pNext = codec_caps_struct;
			caps.pNext = &dec;
		} else {
			enc.sType = VK_STRUCTURE_TYPE_VIDEO_ENCODE_CAPABILITIES_KHR;
			enc.pNext = codec_caps_struct;
			caps.pNext = &enc;
		}
	}
	VkResult r = s->get_video_caps(s->phys, profile, &caps);
	printf("  [%s] vkGetPhysicalDeviceVideoCapabilitiesKHR -> %s\n", label,
	       vkres_str(r));
	if (r != VK_SUCCESS)
		return;
	printf("        coded extent %ux%u..%ux%u, maxDpbSlots=%u maxActiveRefs=%u\n",
	       caps.minCodedExtent.width, caps.minCodedExtent.height,
	       caps.maxCodedExtent.width, caps.maxCodedExtent.height,
	       caps.maxDpbSlots, caps.maxActiveReferencePictures);
	if (codec_caps_struct &&
	    (codec_caps_type == VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_CAPABILITIES_KHR ||
	     codec_caps_type == VK_STRUCTURE_TYPE_VIDEO_DECODE_H265_CAPABILITIES_KHR)) {
		printf("        decode flags=0x%x (coincide=%d distinct=%d)\n",
		       dec.flags,
		       !!(dec.flags & VK_VIDEO_DECODE_CAPABILITY_DPB_AND_OUTPUT_COINCIDE_BIT_KHR),
		       !!(dec.flags & VK_VIDEO_DECODE_CAPABILITY_DPB_AND_OUTPUT_DISTINCT_BIT_KHR));
	} else if (codec_caps_struct) {
		printf("        encode flags=0x%x maxQualityLevels=%u\n", enc.flags,
		       enc.maxQualityLevels);
	}

	VkVideoProfileListInfoKHR list = { 0 };
	list.sType = VK_STRUCTURE_TYPE_VIDEO_PROFILE_LIST_INFO_KHR;
	list.profileCount = 1;
	list.pProfiles = profile;
	VkPhysicalDeviceVideoFormatInfoKHR fi = { 0 };
	fi.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VIDEO_FORMAT_INFO_KHR;
	fi.imageUsage = usage;
	fi.pNext = &list;
	uint32_t n = 0;
	VkResult r2 = s->get_video_formats(s->phys, &fi, &n, NULL);
	printf("        vkGetPhysicalDeviceVideoFormatPropertiesKHR count=%u (%s)\n",
	       n, vkres_str(r2));
	if (n == 0 || (r2 != VK_SUCCESS && r2 != VK_INCOMPLETE))
		return;
	/* one extra stack slot: drivers may return the count one short */
	VkVideoFormatPropertiesKHR *fp = calloc((size_t)n + 4, sizeof(*fp));
	if (!fp)
		return;
	for (uint32_t i = 0; i < n + 4; i++)
		fp[i].sType = VK_STRUCTURE_TYPE_VIDEO_FORMAT_PROPERTIES_KHR;
	uint32_t got = n;
	r2 = s->get_video_formats(s->phys, &fi, &got, fp);
	if (r2 == VK_SUCCESS || r2 == VK_INCOMPLETE) {
		for (uint32_t i = 0; i < got && i < n + 4; i++) {
			printf("        fmt[%u] VkFormat %d, imageCreateFlags=0x%x, "
			       "imageUsageFlags=0x%x\n",
			       i, (int)fp[i].format,
			       (unsigned)fp[i].imageCreateFlags,
			       (unsigned)fp[i].imageUsageFlags);
		}
	}
	free(fp);
}

static void query_video(struct vk_state *s)
{
	printf("\n== Vulkan Video capabilities ==\n");
	if (!s->has_video_queue) {
		printf("  VK_KHR_video_queue not supported: no Vulkan Video on this device/driver.\n");
		return;
	}

	/* ---- H.264 decode: High 4:2:2 10-bit (the 4:2:2 question) ------- */
	if (s->has_video_decode_h264) {
		VkVideoDecodeH264ProfileInfoKHR p422 = { 0 };
		p422.sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_PROFILE_INFO_KHR;
		p422.stdProfileIdc = STD_VIDEO_H264_PROFILE_IDC_HIGH_422;
		p422.pictureLayout = VK_VIDEO_DECODE_H264_PICTURE_LAYOUT_PROGRESSIVE_KHR;
		VkVideoProfileInfoKHR prof = { 0 };
		prof.sType = VK_STRUCTURE_TYPE_VIDEO_PROFILE_INFO_KHR;
		prof.pNext = &p422;
		prof.videoCodecOperation = VK_VIDEO_CODEC_OPERATION_DECODE_H264_BIT_KHR;
		prof.chromaSubsampling = VK_VIDEO_CHROMA_SUBSAMPLING_422_BIT_KHR;
		prof.lumaBitDepth = VK_VIDEO_COMPONENT_BIT_DEPTH_10_BIT_KHR;
		prof.chromaBitDepth = VK_VIDEO_COMPONENT_BIT_DEPTH_10_BIT_KHR;
		VkVideoDecodeH264CapabilitiesKHR hc = { 0 };
		hc.sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_CAPABILITIES_KHR;
		video_caps_and_formats(s, "H.264 decode High4:2:2 10bit", &prof, &hc,
		                       hc.sType, VK_IMAGE_USAGE_VIDEO_DECODE_DST_BIT_KHR);

		VkVideoDecodeH264ProfileInfoKHR p10 = p422;
		p10.stdProfileIdc = STD_VIDEO_H264_PROFILE_IDC_HIGH_10;
		VkVideoProfileInfoKHR prof10 = prof;
		prof10.pNext = &p10;
		prof10.chromaSubsampling = VK_VIDEO_CHROMA_SUBSAMPLING_420_BIT_KHR;
		VkVideoDecodeH264CapabilitiesKHR hc10 = { 0 };
		hc10.sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_CAPABILITIES_KHR;
		video_caps_and_formats(s, "H.264 decode High10 4:2:0 10bit", &prof10,
		                       &hc10, hc10.sType,
		                       VK_IMAGE_USAGE_VIDEO_DECODE_DST_BIT_KHR);

		VkVideoDecodeH264ProfileInfoKHR p8 = p422;
		p8.stdProfileIdc = STD_VIDEO_H264_PROFILE_IDC_HIGH;
		VkVideoProfileInfoKHR prof8 = prof;
		prof8.pNext = &p8;
		prof8.chromaSubsampling = VK_VIDEO_CHROMA_SUBSAMPLING_420_BIT_KHR;
		prof8.lumaBitDepth = VK_VIDEO_COMPONENT_BIT_DEPTH_8_BIT_KHR;
		prof8.chromaBitDepth = VK_VIDEO_COMPONENT_BIT_DEPTH_8_BIT_KHR;
		VkVideoDecodeH264CapabilitiesKHR hc8 = { 0 };
		hc8.sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_H264_CAPABILITIES_KHR;
		video_caps_and_formats(s, "H.264 decode High 4:2:0 8bit", &prof8, &hc8,
		                       hc8.sType,
		                       VK_IMAGE_USAGE_VIDEO_DECODE_DST_BIT_KHR);
	}

	if (s->has_video_decode_h265) {
		VkVideoDecodeH265ProfileInfoKHR p = { 0 };
		p.sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_H265_PROFILE_INFO_KHR;
		p.stdProfileIdc = STD_VIDEO_H265_PROFILE_IDC_MAIN_10;
		VkVideoProfileInfoKHR prof = { 0 };
		prof.sType = VK_STRUCTURE_TYPE_VIDEO_PROFILE_INFO_KHR;
		prof.pNext = &p;
		prof.videoCodecOperation = VK_VIDEO_CODEC_OPERATION_DECODE_H265_BIT_KHR;
		prof.chromaSubsampling = VK_VIDEO_CHROMA_SUBSAMPLING_420_BIT_KHR;
		prof.lumaBitDepth = VK_VIDEO_COMPONENT_BIT_DEPTH_10_BIT_KHR;
		prof.chromaBitDepth = VK_VIDEO_COMPONENT_BIT_DEPTH_10_BIT_KHR;
		VkVideoDecodeH265CapabilitiesKHR hc = { 0 };
		hc.sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_H265_CAPABILITIES_KHR;
		video_caps_and_formats(s, "HEVC decode Main10 4:2:0 10bit", &prof, &hc,
		                       hc.sType,
		                       VK_IMAGE_USAGE_VIDEO_DECODE_DST_BIT_KHR);

		VkVideoDecodeH265ProfileInfoKHR p422 = p;
		p422.stdProfileIdc = STD_VIDEO_H265_PROFILE_IDC_FORMAT_RANGE_EXTENSIONS;
		VkVideoProfileInfoKHR prof422 = prof;
		prof422.pNext = &p422;
		prof422.chromaSubsampling = VK_VIDEO_CHROMA_SUBSAMPLING_422_BIT_KHR;
		VkVideoDecodeH265CapabilitiesKHR hc422 = { 0 };
		hc422.sType = VK_STRUCTURE_TYPE_VIDEO_DECODE_H265_CAPABILITIES_KHR;
		video_caps_and_formats(s, "HEVC decode RExt 4:2:2 10bit", &prof422,
		                       &hc422, hc422.sType,
		                       VK_IMAGE_USAGE_VIDEO_DECODE_DST_BIT_KHR);
	}

	/* ---- encode: H.264 High/High10/High422, HEVC Main10/RExt -------- */
	if (s->has_video_encode_h264) {
		const struct { int idc; const char *name; } profs[] = {
			{ STD_VIDEO_H264_PROFILE_IDC_HIGH, "High 4:2:0 8bit" },
			{ STD_VIDEO_H264_PROFILE_IDC_HIGH_10, "High10 4:2:0 10bit" },
			{ STD_VIDEO_H264_PROFILE_IDC_HIGH_422, "High422 4:2:2 10bit" },
		};
		for (size_t i = 0; i < sizeof(profs) / sizeof(profs[0]); i++) {
			VkVideoEncodeH264ProfileInfoKHR p = { 0 };
			p.sType = VK_STRUCTURE_TYPE_VIDEO_ENCODE_H264_PROFILE_INFO_KHR;
			p.stdProfileIdc = profs[i].idc;
			VkVideoProfileInfoKHR prof = { 0 };
			prof.sType = VK_STRUCTURE_TYPE_VIDEO_PROFILE_INFO_KHR;
			prof.pNext = &p;
			prof.videoCodecOperation =
				VK_VIDEO_CODEC_OPERATION_ENCODE_H264_BIT_KHR;
			prof.chromaSubsampling =
				profs[i].idc == STD_VIDEO_H264_PROFILE_IDC_HIGH_422
					? VK_VIDEO_CHROMA_SUBSAMPLING_422_BIT_KHR
					: VK_VIDEO_CHROMA_SUBSAMPLING_420_BIT_KHR;
			prof.lumaBitDepth =
				profs[i].idc == STD_VIDEO_H264_PROFILE_IDC_HIGH
					? VK_VIDEO_COMPONENT_BIT_DEPTH_8_BIT_KHR
					: VK_VIDEO_COMPONENT_BIT_DEPTH_10_BIT_KHR;
			prof.chromaBitDepth = prof.lumaBitDepth;
			VkVideoEncodeH264CapabilitiesKHR hc = { 0 };
			hc.sType = VK_STRUCTURE_TYPE_VIDEO_ENCODE_H264_CAPABILITIES_KHR;
			char label[96];
			snprintf(label, sizeof(label), "H.264 encode %s", profs[i].name);
			video_caps_and_formats(s, label, &prof, &hc, hc.sType,
			                       VK_IMAGE_USAGE_VIDEO_ENCODE_SRC_BIT_KHR);
		}
	}
	if (s->has_video_encode_h265) {
		const struct { int idc; const char *name; } profs[] = {
			{ STD_VIDEO_H265_PROFILE_IDC_MAIN_10, "Main10 4:2:0 10bit" },
			{ STD_VIDEO_H265_PROFILE_IDC_FORMAT_RANGE_EXTENSIONS, "RExt 4:2:2 10bit" },
		};
		for (size_t i = 0; i < sizeof(profs) / sizeof(profs[0]); i++) {
			VkVideoEncodeH265ProfileInfoKHR p = { 0 };
			p.sType = VK_STRUCTURE_TYPE_VIDEO_ENCODE_H265_PROFILE_INFO_KHR;
			p.stdProfileIdc = profs[i].idc;
			VkVideoProfileInfoKHR prof = { 0 };
			prof.sType = VK_STRUCTURE_TYPE_VIDEO_PROFILE_INFO_KHR;
			prof.pNext = &p;
			prof.videoCodecOperation =
				VK_VIDEO_CODEC_OPERATION_ENCODE_H265_BIT_KHR;
			prof.chromaSubsampling =
				profs[i].idc == STD_VIDEO_H265_PROFILE_IDC_FORMAT_RANGE_EXTENSIONS
					? VK_VIDEO_CHROMA_SUBSAMPLING_422_BIT_KHR
					: VK_VIDEO_CHROMA_SUBSAMPLING_420_BIT_KHR;
			prof.lumaBitDepth = VK_VIDEO_COMPONENT_BIT_DEPTH_10_BIT_KHR;
			prof.chromaBitDepth = prof.lumaBitDepth;
			VkVideoEncodeH265CapabilitiesKHR hc = { 0 };
			hc.sType = VK_STRUCTURE_TYPE_VIDEO_ENCODE_H265_CAPABILITIES_KHR;
			char label[96];
			snprintf(label, sizeof(label), "HEVC encode %s", profs[i].name);
			video_caps_and_formats(s, label, &prof, &hc, hc.sType,
			                       VK_IMAGE_USAGE_VIDEO_ENCODE_SRC_BIT_KHR);
		}
	}
}

/* ------------------------------------------------------------------ */
/* Vulkan external-memory capabilities for linear plane formats        */
/* ------------------------------------------------------------------ */

static PFN_vkGetPhysicalDeviceImageFormatProperties2 g_imgfmt2;
static PFN_vkGetPhysicalDeviceExternalBufferProperties g_extbufprops;

static void check_linear_import_caps(struct vk_state *s)
{
	printf("\n== Vulkan external-memory import capability (DMA-BUF, linear) ==\n");
	if (!s->has_ext_dma_buf || !s->has_ext_drm_modifier) {
		printf("  missing VK_EXT_external_memory_dma_buf and/or "
		       "VK_EXT_image_drm_format_modifier -> dma-buf import path is "
		       "unavailable on this driver\n");
		return;
	}
	g_imgfmt2 = (PFN_vkGetPhysicalDeviceImageFormatProperties2)
		vkGetInstanceProcAddr(s->instance,
		                      "vkGetPhysicalDeviceImageFormatProperties2");
	g_extbufprops = (PFN_vkGetPhysicalDeviceExternalBufferProperties)
		vkGetInstanceProcAddr(s->instance,
		                      "vkGetPhysicalDeviceExternalBufferProperties");
	if (!g_imgfmt2) {
		printf("  vkGetPhysicalDeviceImageFormatProperties2 missing\n");
		return;
	}

	VkPhysicalDeviceExternalBufferInfo bi = { 0 };
	bi.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_BUFFER_INFO;
	bi.usage = VK_BUFFER_USAGE_TRANSFER_SRC_BIT;
	bi.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;
	VkExternalBufferProperties bp = { 0 };
	bp.sType = VK_STRUCTURE_TYPE_EXTERNAL_BUFFER_PROPERTIES;
	if (g_extbufprops) {
		g_extbufprops(s->phys, &bi, &bp);
		printf("  buffer DMA_BUF: features=0x%x (dedicatedOnly=%d exportable=%d "
		       "importable=%d) compatibleHandleTypes=0x%x\n",
		       bp.externalMemoryProperties.externalMemoryFeatures,
		       !!(bp.externalMemoryProperties.externalMemoryFeatures &
		          VK_EXTERNAL_MEMORY_FEATURE_DEDICATED_ONLY_BIT),
		       !!(bp.externalMemoryProperties.externalMemoryFeatures &
		          VK_EXTERNAL_MEMORY_FEATURE_EXPORTABLE_BIT),
		       !!(bp.externalMemoryProperties.externalMemoryFeatures &
		          VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT),
		       bp.externalMemoryProperties.compatibleHandleTypes);
	}

	struct {
		VkFormat fmt;
		const char *name;
	} fmts[] = {
		{ VK_FORMAT_R8_UNORM, "R8_UNORM (NV12/P010 luma)" },
		{ VK_FORMAT_R8G8_UNORM, "R8G8_UNORM (NV12 chroma)" },
		{ VK_FORMAT_R16_UNORM, "R16_UNORM (P010/P210 luma)" },
		{ VK_FORMAT_R16G16_UNORM, "R16G16_UNORM (P010/P210 chroma)" },
		{ VK_FORMAT_G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
		  "G10X6..2PLANE_420 (P010 native)" },
		{ VK_FORMAT_G10X6_B10X6R10X6_2PLANE_422_UNORM_3PACK16,
		  "G10X6..2PLANE_422 (P210 native)" },
	};
	for (size_t i = 0; i < sizeof(fmts) / sizeof(fmts[0]); i++) {
		VkPhysicalDeviceImageDrmFormatModifierInfoEXT mod = { 0 };
		mod.sType =
			VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_DRM_FORMAT_MODIFIER_INFO_EXT;
		mod.drmFormatModifier = 0; /* DRM_FORMAT_MOD_LINEAR */
		mod.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
		VkPhysicalDeviceExternalImageFormatInfo ext = { 0 };
		ext.sType =
			VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO;
		ext.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;
		ext.pNext = &mod;
		VkPhysicalDeviceImageFormatInfo2 fi = { 0 };
		fi.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2;
		fi.format = fmts[i].fmt;
		fi.type = VK_IMAGE_TYPE_2D;
		fi.tiling = VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT;
		fi.usage = VK_IMAGE_USAGE_SAMPLED_BIT;
		fi.pNext = &ext;
		VkExternalImageFormatProperties exp = { 0 };
		exp.sType = VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES;
		VkImageFormatProperties2 ip = { 0 };
		ip.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2;
		ip.pNext = &exp;
		VkResult r = g_imgfmt2(s->phys, &fi, &ip);
		printf("  %-34s tiling=DRM_MODIFIER(LINEAR) -> %s, "
		       "extMemFeatures=0x%x (importable=%d)\n",
		       fmts[i].name, vkres_str(r),
		       exp.externalMemoryProperties.externalMemoryFeatures,
		       !!(exp.externalMemoryProperties.externalMemoryFeatures &
		          VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT));
		if (r != VK_SUCCESS)
			continue;
		/* linear tiling variant (no DRM modifier chain) */
		VkPhysicalDeviceExternalImageFormatInfo ext2 = { 0 };
		ext2.sType =
			VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO;
		ext2.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;
		VkPhysicalDeviceImageFormatInfo2 fi2 = fi;
		fi2.tiling = VK_IMAGE_TILING_LINEAR;
		fi2.pNext = &ext2;
		VkExternalImageFormatProperties exp2 = { 0 };
		exp2.sType = VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES;
		VkImageFormatProperties2 ip2 = { 0 };
		ip2.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2;
		ip2.pNext = &exp2;
		VkResult r2 = g_imgfmt2(s->phys, &fi2, &ip2);
		printf("  %-34s tiling=LINEAR                 -> %s, "
		       "extMemFeatures=0x%x (importable=%d)\n",
		       fmts[i].name, vkres_str(r2),
		       exp2.externalMemoryProperties.externalMemoryFeatures,
		       !!(exp2.externalMemoryProperties.externalMemoryFeatures &
		          VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT));
		/* route B: the same linear image imported from an OPAQUE_FD
		 * (CUDA VMM POSIX_FD export) rather than a dma-buf. */
		VkPhysicalDeviceExternalImageFormatInfo ext3 = { 0 };
		ext3.sType =
			VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO;
		ext3.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT;
		VkPhysicalDeviceImageFormatInfo2 fi3 = fi;
		fi3.tiling = VK_IMAGE_TILING_LINEAR;
		fi3.pNext = &ext3;
		VkExternalImageFormatProperties exp3 = { 0 };
		exp3.sType = VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES;
		VkImageFormatProperties2 ip3 = { 0 };
		ip3.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2;
		ip3.pNext = &exp3;
		VkResult r3 = g_imgfmt2(s->phys, &fi3, &ip3);
		printf("  %-34s tiling=LINEAR OPAQUE_FD     -> %s, "
		       "extMemFeatures=0x%x (importable=%d)\n",
		       fmts[i].name, vkres_str(r3),
		       exp3.externalMemoryProperties.externalMemoryFeatures,
		       !!(exp3.externalMemoryProperties.externalMemoryFeatures &
		          VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT));
	}
}

/* ------------------------------------------------------------------ */
/* CUDA: dma-buf export                                                */
/* ------------------------------------------------------------------ */

struct cuda_probe {
	int ok;
	CUdevice dev;
	CUcontext ctx;
	char name[256];
	int attr_texture_alignment;
	int attr_dma_buf;
	int attr_host_dma_buf;
	int attr_mmap;
	/* exported test allocation */
	CUdeviceptr ptr;
	size_t size;      /* page-aligned export size */
	size_t alloc_size;
	uint32_t width, height, pitch;
	int fd;           /* dma-buf fd, -1 when not exported */
};

static int cuda_probe_run(struct cuda_api *cu, struct cuda_probe *p)
{
	memset(p, 0, sizeof(*p));
	p->fd = -1;
	if (!cu->init) {
		printf("[cuda] driver API missing\n");
		return 0;
	}
	CUresult r = cu->init(0);
	if (r != CUDA_SUCCESS) {
		printf("[cuda] cuInit -> %s (%s)\n", cuda_err_name(cu, r),
		       cuda_err_string(cu, r));
		return 0;
	}
	r = cu->device_get(&p->dev, 0);
	if (r != CUDA_SUCCESS) {
		printf("[cuda] cuDeviceGet -> %s\n", cuda_err_name(cu, r));
		return 0;
	}
	if (cu->device_name)
		cu->device_name(p->name, sizeof(p->name), p->dev);
	else
		snprintf(p->name, sizeof(p->name), "(unknown)");
	int attrs[3] = { 0, 0, 0 };
	cu->device_attr(&attrs[0], CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT, p->dev);
	cu->device_attr(&attrs[1], CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED, p->dev);
	cu->device_attr(&attrs[2], CU_DEVICE_ATTRIBUTE_DMA_BUF_MMAP_SUPPORTED, p->dev);
	p->attr_texture_alignment = attrs[0];
	p->attr_dma_buf = attrs[1];
	p->attr_mmap = attrs[2];
	int host_dma = 0;
	cu->device_attr(&host_dma, CU_DEVICE_ATTRIBUTE_HOST_ALLOC_DMA_BUF_SUPPORTED,
	                p->dev);
	p->attr_host_dma_buf = host_dma;

	printf("\n== CUDA ==\n");
	printf("[cuda] device 0: %s\n", p->name);
	printf("[cuda] CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT          = %d\n",
	       p->attr_texture_alignment);
	printf("[cuda] CU_DEVICE_ATTRIBUTE_DMA_BUF_SUPPORTED          = %d  <-- "
	       "%s\n", p->attr_dma_buf,
	       p->attr_dma_buf ? "ffmpeg cuMemAlloc frames ARE exportable"
	                       : "NO dma-buf export on this device/driver");
	printf("[cuda] CU_DEVICE_ATTRIBUTE_HOST_ALLOC_DMA_BUF_SUPPORTED = %d\n",
	       p->attr_host_dma_buf);
	printf("[cuda] CU_DEVICE_ATTRIBUTE_DMA_BUF_MMAP_SUPPORTED     = %d\n",
	       p->attr_mmap);

	r = cu->ctx_create(&p->ctx, 0, p->dev);
	if (r != CUDA_SUCCESS) {
		printf("[cuda] cuCtxCreate -> %s\n", cuda_err_name(cu, r));
		return 0;
	}
	if (cu->ctx_push)
		cu->ctx_push(p->ctx);

	/* A P010 4:2:0 10-bit frame: 1920x1080 luma (16-bit) + half-size
	 * chroma, mimicking FFmpeg's single-buffer frame layout. FFmpeg
	 * computes the size with av_image_get_buffer_size(sw_format, w, h,
	 * tex_alignment) and allocates it with cuMemAlloc (hwcontext_cuda.c:
	 * cuda_pool_alloc). */
	p->width = 1920;
	p->height = 1080;
	size_t luma = (size_t)p->width * 2 * p->height;
	size_t chroma = (size_t)p->width * 2 * (p->height / 2);
	p->alloc_size = luma + chroma;
	size_t page = 4096;
	p->size = (p->alloc_size + page - 1) & ~(page - 1);

	r = cu->mem_alloc(&p->ptr, p->size + page); /* slack for alignment test */
	if (r != CUDA_SUCCESS) {
		printf("[cuda] cuMemAlloc(%zu) -> %s\n", p->size + page,
		       cuda_err_name(cu, r));
		if (cu->ctx_pop)
			cu->ctx_pop(&p->ctx);
		return 0;
	}
	uint64_t base = p->ptr;
	printf("[cuda] cuMemAlloc: ptr=0x%llx (page-aligned=%d, %%4096=%llu), "
	       "alloc=%zu, page-rounded=%zu\n",
	       (unsigned long long)base, (base % page) == 0,
	       (unsigned long long)(base % page), p->alloc_size, p->size);

	/* Export: (a) exactly [ptr, page_rounded) as FFmpeg would need it if
	 * the base is page aligned; (b) the enclosing page-aligned range
	 * when it is not. */
	struct {
		const char *what;
		CUdeviceptr dptr;
		size_t size;
		unsigned long long flags;
	} tries[3] = {
		{ "exact [ptr, round_up(size)]", base, p->size, 0 },
		{ "page-aligned subrange", base & ~(uint64_t)(page - 1),
		  p->size, 0 },
		{ "exact range, PCIE BAR1 flag", base, p->size,
		  CU_MEM_RANGE_FLAG_DMA_BUF_MAPPING_TYPE_PCIE },
	};
	/* also probe the shapes a page-aligned pool would use */
	tries[1].dptr = base & ~(uint64_t)(page - 1);
	tries[1].size = p->size;
	for (size_t i = 0; i < sizeof(tries) / sizeof(tries[0]); i++) {
		int fd = -1;
		r = cu->mem_handle_range(&fd, tries[i].dptr, tries[i].size,
		                         CU_MEM_RANGE_HANDLE_TYPE_DMA_BUF_FD,
		                         tries[i].flags);
		printf("[cuda] cuMemGetHandleForAddressRange(%-28s dptr=0x%llx "
		       "size=%zu flags=0x%llx) -> %s\n",
		       tries[i].what, (unsigned long long)tries[i].dptr,
		       tries[i].size, tries[i].flags, cuda_err_name(cu, r));
		if (r == CUDA_SUCCESS) {
			printf("        fd=%d\n", fd);
			if (p->fd < 0)
				p->fd = fd;
			else
				close(fd);
		}
	}
	if (cu->ctx_pop) {
		CUcontext dummy = NULL;
		cu->ctx_pop(&dummy);
	}
	p->ok = 1;
	return 1;
}


/* ------------------------------------------------------------------ */
/* Route B: exportable CUDA VMM pool (cuMemCreate + POSIX_FD)          */
/* ------------------------------------------------------------------ */

struct cuda_vmm_result {
	int vmm_supported;
	int posix_fd_supported;
	size_t gran_min;
	size_t gran_rec;
	int created;
	int mapped;
	int exported_fd;  /* cuMemExportToShareableHandle */
	int range_fd;     /* cuMemGetHandleForAddressRange on the mapped VA */
	uint64_t va;
	size_t size;
	CUmemGenericAllocationHandle handle;
	char export_kind[128];
	char range_kind[128];
};

/* What kind of kernel object is behind an fd (dma-buf / driver-private /
 * something else)? */
static void fd_kind(int fd, char *out, size_t out_len)
{
	char path[64];
	char buf[256];
	snprintf(path, sizeof(path), "/proc/self/fd/%d", fd);
	ssize_t n = readlink(path, buf, sizeof(buf) - 1);
	if (n < 0) {
		snprintf(out, out_len, "readlink failed: %s", strerror(errno));
		return;
	}
	buf[n] = 0;
	snprintf(out, out_len, "%.*s", (int)(out_len - 1), buf);
}

static void cuda_vmm_cleanup(struct cuda_api *cu, struct cuda_vmm_result *r)
{
	if (r->exported_fd >= 0) {
		close(r->exported_fd);
		r->exported_fd = -1;
	}
	if (r->range_fd >= 0) {
		close(r->range_fd);
		r->range_fd = -1;
	}
	if (r->mapped && cu->mem_unmap) {
		cu->mem_unmap(r->va, r->size);
		r->mapped = 0;
	}
	if (r->va && cu->addr_free) {
		cu->addr_free(r->va, r->size);
		r->va = 0;
	}
	if (r->created && cu->mem_release) {
		cu->mem_release(r->handle);
		r->created = 0;
	}
}

/* Returns 1 when the path is structurally testable (VMM create+map+export
 * attempted), 0 when the driver lacks VMM. */
static int cuda_route_b(struct cuda_api *cu, struct cuda_probe *p,
                        struct cuda_vmm_result *r)
{
	memset(r, 0, sizeof(*r));
	r->exported_fd = -1;
	r->range_fd = -1;
	int attrs[2] = { 0, 0 };
	cu->device_attr(&attrs[0], CU_DEVICE_ATTRIBUTE_VIRTUAL_MEMORY_MANAGEMENT_SUPPORTED,
	                p->dev);
	cu->device_attr(&attrs[1],
	                CU_DEVICE_ATTRIBUTE_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR_SUPPORTED,
	                p->dev);
	r->vmm_supported = attrs[0];
	r->posix_fd_supported = attrs[1];

	printf("\n== CUDA route B: virtual memory management ==\n");
	printf("[vmm] CU_DEVICE_ATTRIBUTE_VIRTUAL_MEMORY_MANAGEMENT_SUPPORTED = %d\n",
	       r->vmm_supported);
	printf("[vmm] CU_DEVICE_ATTRIBUTE_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR_SUPPORTED = %d\n",
	       r->posix_fd_supported);
	if (!cu->granularity || !cu->mem_create || !cu->mem_map ||
	    !cu->mem_set_access || !cu->addr_reserve) {
		printf("[vmm] SKIP: driver lacks the VMM entry points\n");
		return 0;
	}
	if (!r->vmm_supported) {
		printf("[vmm] SKIP: VMM not supported by this device\n");
		return 0;
	}

	CUresult pc = cu->ctx_push ? cu->ctx_push(p->ctx) : CUDA_SUCCESS;
	CUresult sc = cu->ctx_set_current ? cu->ctx_set_current(p->ctx) : CUDA_SUCCESS;
	int cur_dev = -1;
	CUresult dc = cu->ctx_get_device ? cu->ctx_get_device(&cur_dev) : CUDA_SUCCESS;
	printf("[vmm] context: push=%s set_current=%s get_device=%s device=%d "
	       "(ctx=%p)\n", cuda_err_name(cu, pc), cuda_err_name(cu, sc),
	       cuda_err_name(cu, dc), cur_dev, p->ctx);
	if (cur_dev < 0)
		cur_dev = 0;

	struct cu_mem_alloc_prop prop;
	memset(&prop, 0, sizeof(prop));
	prop.type = CU_MEM_ALLOCATION_TYPE_PINNED;
	prop.requested_handle_types = CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR;
	prop.location_type = CU_MEM_LOCATION_TYPE_DEVICE;
	prop.location_id = cur_dev;

	CUresult rc = cu->granularity(&r->gran_min, &prop,
	                              CU_MEM_ALLOC_GRANULARITY_MINIMUM);
	CUresult rc2 = cu->granularity(&r->gran_rec, &prop,
	                               CU_MEM_ALLOC_GRANULARITY_RECOMMENDED);
	printf("[vmm] cuMemGetAllocationGranularity(min) -> %s, granularity=%zu\n",
	       cuda_err_name(cu, rc), r->gran_min);
	printf("[vmm] cuMemGetAllocationGranularity(rec) -> %s, granularity=%zu\n",
	       cuda_err_name(cu, rc2), r->gran_rec);
	if (rc != CUDA_SUCCESS) {
		/* Some drivers reject an unsupported handle type in the prop
		 * before any allocation is attempted; retry with no handle
		 * type so the VMM path itself is still measured. */
		prop.requested_handle_types = 0;
		rc = cu->granularity(&r->gran_min, &prop,
		                     CU_MEM_ALLOC_GRANULARITY_MINIMUM);
		rc2 = cu->granularity(&r->gran_rec, &prop,
		                      CU_MEM_ALLOC_GRANULARITY_RECOMMENDED);
		printf("[vmm] cuMemGetAllocationGranularity(no handle type) -> %s, "
		       "min=%zu rec=%zu\n", cuda_err_name(cu, rc), r->gran_min,
		       r->gran_rec);
		if (rc != CUDA_SUCCESS) {
			if (cu->ctx_pop)
				cu->ctx_pop(&p->ctx);
			return 0;
		}
		/* keep POSIX_FD for the create attempt: it tells whether the
		 * handle type or VMM itself is the blocker */
		prop.requested_handle_types = CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR;
	}
	size_t size = p->size; /* P010 1080p-sized, page-rounded */
	size_t gran = r->gran_min ? r->gran_min : 4096;
	size = (size + gran - 1) / gran * gran;
	r->size = size;

	CUmemGenericAllocationHandle handle = 0;
	rc = cu->mem_create(&handle, size, &prop, 0);
	printf("[vmm] cuMemCreate(size=%zu, requestedHandleTypes=POSIX_FD) -> %s\n",
	       size, cuda_err_name(cu, rc));
	if (rc != CUDA_SUCCESS) {
		/* distinguish "VMM works but fd export is refused" from "no VMM" */
		prop.requested_handle_types = 0;
		rc = cu->mem_create(&handle, size, &prop, 0);
		printf("[vmm] cuMemCreate(requestedHandleTypes=NONE) -> %s\n",
		       cuda_err_name(cu, rc));
		if (rc != CUDA_SUCCESS) {
			if (cu->ctx_pop)
				cu->ctx_pop(&p->ctx);
			return 0;
		}
	}
	r->handle = handle;
	r->created = 1;
	if (cu->alloc_props) {
		struct cu_mem_alloc_prop got;
		memset(&got, 0, sizeof(got));
		CUresult rg = cu->alloc_props(&got, handle);
		printf("[vmm] cuMemGetAllocationPropertiesFromHandle -> %s, "
		       "requestedHandleTypes=0x%x\n", cuda_err_name(cu, rg),
		       got.requested_handle_types);
	}

	uint64_t va = 0;
	rc = cu->addr_reserve(&va, size, 0, 0, 0);
	printf("[vmm] cuMemAddressReserve(size=%zu) -> %s, va=0x%llx\n", size,
	       cuda_err_name(cu, rc), (unsigned long long)va);
	if (rc != CUDA_SUCCESS) {
		cuda_vmm_cleanup(cu, r);
		if (cu->ctx_pop)
			cu->ctx_pop(&p->ctx);
		return 0;
	}
	r->va = va;

	rc = cu->mem_map(va, size, 0, handle, 0);
	printf("[vmm] cuMemMap(va=0x%llx, size=%zu) -> %s\n",
	       (unsigned long long)va, size, cuda_err_name(cu, rc));
	if (rc != CUDA_SUCCESS) {
		cuda_vmm_cleanup(cu, r);
		if (cu->ctx_pop)
			cu->ctx_pop(&p->ctx);
		return 0;
	}
	r->mapped = 1;

	struct cu_mem_access_desc access;
	memset(&access, 0, sizeof(access));
	access.location_type = CU_MEM_LOCATION_TYPE_DEVICE;
	access.location_id = cur_dev;
	access.flags = CU_MEM_ACCESS_FLAGS_PROT_READWRITE;
	rc = cu->mem_set_access(va, size, &access, 1);
	printf("[vmm] cuMemSetAccess(READWRITE) -> %s (%s)\n",
	       cuda_err_name(cu, rc), cuda_err_string(cu, rc));

	unsigned char pattern[256];
	memset(pattern, 0xAB, sizeof(pattern));

	/* Control: prove the copy mechanism itself works on ordinary
	 * cuMemAlloc memory in this context (cuMemsetD8 returned
	 * CUDA_ERROR_LAUNCH_FAILED on the VMM range in the first GPU run;
	 * this separates "copy broken" from "VMM not accessible"). */
	if (cu->mem_alloc && cu->memcpy_htod) {
		CUdeviceptr ctrl = 0;
		CUresult cr = cu->mem_alloc(&ctrl, sizeof(pattern));
		CUresult wr = cr;
		if (cr == CUDA_SUCCESS)
			wr = cu->memcpy_htod(ctrl, pattern, sizeof(pattern));
		CUresult sr = cu->ctx_sync ? cu->ctx_sync() : CUDA_SUCCESS;
		printf("[vmm] control cuMemAlloc+cuMemcpyHtoD(0xAB) -> alloc=%s "
		       "copy=%s sync=%s (%s)\n", cuda_err_name(cu, cr),
		       cuda_err_name(cu, wr), cuda_err_name(cu, sr),
		       cuda_err_string(cu, wr));
		if (cr == CUDA_SUCCESS)
			cu->mem_free(ctrl);
	}

	/* Fill the VMM range with the known pattern (the import test reads it
	 * back). The context can be left non-current by earlier calls (FFmpeg
	 * does this around its own operations), so re-assert it here. */
	if (cu->ctx_set_current) {
		CUresult rsc = cu->ctx_set_current(p->ctx);
		printf("[vmm] re-assert ctx before copy: %s\n", cuda_err_name(cu, rsc));
	}
	if (cu->memcpy_htod) {
		rc = cu->memcpy_htod(va, pattern, sizeof(pattern));
		printf("[vmm] cuMemcpyHtoD(va, 0xAB, 256) -> %s (%s)\n",
		       cuda_err_name(cu, rc), cuda_err_string(cu, rc));
		if (cu->ctx_sync) {
			CUresult sr = cu->ctx_sync();
			printf("[vmm] cuCtxSynchronize -> %s (%s)\n",
			       cuda_err_name(cu, sr), cuda_err_string(cu, sr));
		}
	}
	if (cu->memset_d8) {
		CUresult mr = cu->memset_d8(va, 0xAB, 256);
		printf("[vmm] cuMemsetD8(va, 0xAB, 256) -> %s (%s) [diagnostic]\n",
		       cuda_err_name(cu, mr), cuda_err_string(cu, mr));
	}

	/* (1) export the allocation handle itself */
	int fd = -1;
	rc = cu->mem_export(&fd, handle, CU_MEM_HANDLE_TYPE_POSIX_FILE_DESCRIPTOR, 0);
	printf("[vmm] cuMemExportToShareableHandle(POSIX_FD) -> %s",
	       cuda_err_name(cu, rc));
	if (rc == CUDA_SUCCESS) {
		fd_kind(fd, r->export_kind, sizeof(r->export_kind));
		printf(", fd=%d (%s)", fd, r->export_kind);
		r->exported_fd = fd;
	}
	printf("\n");

	/* (2) export the mapped VA range (documented DMA-BUF path) */
	int fd2 = -1;
	if (cu->mem_handle_range) {
		rc = cu->mem_handle_range(&fd2, va, size,
		                          CU_MEM_RANGE_HANDLE_TYPE_DMA_BUF_FD, 0);
		printf("[vmm] cuMemGetHandleForAddressRange(mapped VA, DMA_BUF_FD) "
		       "-> %s", cuda_err_name(cu, rc));
		if (rc == CUDA_SUCCESS) {
			fd_kind(fd2, r->range_kind, sizeof(r->range_kind));
			printf(", fd=%d (%s)", fd2, r->range_kind);
			r->range_fd = fd2;
		}
		printf("\n");
	}

	if (cu->ctx_pop) {
		CUcontext dummy = NULL;
		cu->ctx_pop(&dummy);
	}
	return 1;
}

/* ------------------------------------------------------------------ */
/* Vulkan: import the CUDA dma-buf as an image, read it back           */
/* ------------------------------------------------------------------ */

static uint32_t find_memory_type(VkPhysicalDevice phys, uint32_t bits,
                                 VkMemoryPropertyFlags want)
{
	VkPhysicalDeviceMemoryProperties mp;
	vkGetPhysicalDeviceMemoryProperties(phys, &mp);
	for (uint32_t i = 0; i < mp.memoryTypeCount; i++) {
		if ((bits & (1u << i)) &&
		    (mp.memoryTypes[i].propertyFlags & want) == want)
			return i;
	}
	return UINT32_MAX;
}

static int vk_import_test(struct vk_state *s, int fd, uint32_t width,
                          uint32_t height, uint32_t pitch, const char *label,
                          VkImageTiling tiling, VkExternalMemoryHandleTypeFlagBits ht)
{
	if (fd < 0 || !s->dev) {
		printf("  [%s] SKIP (no fd or no device)\n", label);
		return 0;
	}
	VkExternalMemoryImageCreateInfo ext = { 0 };
	ext.sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO;
	ext.handleTypes = ht;

	VkSubresourceLayout layout = { 0 };
	layout.offset = 0;
	layout.size = 0;
	layout.rowPitch = pitch;
	VkImageDrmFormatModifierExplicitCreateInfoEXT mod = { 0 };
	mod.sType = VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_EXPLICIT_CREATE_INFO_EXT;
	mod.drmFormatModifier = 0; /* linear */
	mod.drmFormatModifierPlaneCount = 1;
	mod.pPlaneLayouts = &layout;

	if (tiling == VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT) {
		mod.pNext = &ext;
	} else {
		ext.pNext = NULL;
	}

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
	ici.tiling = tiling;
	ici.usage = VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
	ici.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
	ici.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
	if (tiling == VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT)
		ici.pNext = &mod;
	else
		ici.pNext = &ext;

	VkImage image = VK_NULL_HANDLE;
	VkResult r = vkCreateImage(s->dev, &ici, NULL, &image);
	printf("  [%s] vkCreateImage -> %s\n", label, vkres_str(r));
	if (r != VK_SUCCESS)
		return 0;

	VkMemoryRequirements req;
	vkGetImageMemoryRequirements(s->dev, image, &req);
	int dupfd = fcntl(fd, F_DUPFD_CLOEXEC, 3);
	if (dupfd < 0) {
		printf("  [%s] dup failed\n", label);
		vkDestroyImage(s->dev, image, NULL);
		return 0;
	}
	uint32_t mt = find_memory_type(s->phys, req.memoryTypeBits,
	                               VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
	if (mt == UINT32_MAX)
		mt = find_memory_type(s->phys, req.memoryTypeBits, 0);
	VkMemoryDedicatedAllocateInfo ded = { 0 };
	ded.sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO;
	ded.image = image;
	VkImportMemoryFdInfoKHR imp = { 0 };
	imp.sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR;
	imp.handleType = ht;
	imp.fd = dupfd;
	imp.pNext = &ded;
	VkMemoryAllocateInfo mai = { 0 };
	mai.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
	mai.allocationSize = req.size;
	mai.memoryTypeIndex = mt;
	mai.pNext = &imp;
	VkDeviceMemory mem = VK_NULL_HANDLE;
	VkResult r2 = vkAllocateMemory(s->dev, &mai, NULL, &mem);
	printf("  [%s] vkAllocateMemory(import %s) -> %s\n", label,
	       ht == VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT ? "DMA_BUF"
	                                                             : "OPAQUE_FD",
	       vkres_str(r2));
	if (r2 != VK_SUCCESS) {
		close(dupfd);
		vkDestroyImage(s->dev, image, NULL);
		return 0;
	}
	r = vkBindImageMemory(s->dev, image, mem, 0);
	printf("  [%s] vkBindImageMemory -> %s\n", label, vkres_str(r));

	/* read back: image -> buffer -> host */
	VkBufferCreateInfo bci = { 0 };
	bci.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO;
	bci.size = 64;
	bci.usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT;
	bci.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
	VkBuffer buf = VK_NULL_HANDLE;
	vkCreateBuffer(s->dev, &bci, NULL, &buf);
	VkMemoryRequirements breq;
	vkGetBufferMemoryRequirements(s->dev, buf, &breq);
	uint32_t bmt = find_memory_type(s->phys, breq.memoryTypeBits,
	                                VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
	                                VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
	VkMemoryAllocateInfo bmai = { 0 };
	bmai.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
	bmai.allocationSize = breq.size;
	bmai.memoryTypeIndex = bmt;
	VkDeviceMemory bmem = VK_NULL_HANDLE;
	vkAllocateMemory(s->dev, &bmai, NULL, &bmem);
	vkBindBufferMemory(s->dev, buf, bmem, 0);

	VkCommandPoolCreateInfo pci = { 0 };
	pci.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO;
	pci.queueFamilyIndex = s->queue_family;
	VkCommandPool pool = VK_NULL_HANDLE;
	vkCreateCommandPool(s->dev, &pci, NULL, &pool);
	VkCommandBufferAllocateInfo cai = { 0 };
	cai.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO;
	cai.commandPool = pool;
	cai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
	cai.commandBufferCount = 1;
	VkCommandBuffer cmd;
	vkAllocateCommandBuffers(s->dev, &cai, &cmd);
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
	b.srcAccessMask = 0;
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
	VkResult r3 = vkQueueSubmit(s->queue, 1, &si, VK_NULL_HANDLE);
	if (r3 == VK_SUCCESS)
		r3 = vkQueueWaitIdle(s->queue);
	printf("  [%s] image->buffer copy -> %s\n", label, vkres_str(r3));
	int ok_pattern = 0;
	if (r3 == VK_SUCCESS) {
		void *mapped = NULL;
		if (vkMapMemory(s->dev, bmem, 0, 64, 0, &mapped) == VK_SUCCESS) {
			unsigned char *p = mapped;
			printf("        first bytes:");
			for (int i = 0; i < 8; i++)
				printf(" %02x", p[i]);
			ok_pattern = p[0] == 0xAB;
			printf("  -> %s\n", ok_pattern ? "MATCH (0xab)" : "MISMATCH");
			vkUnmapMemory(s->dev, bmem);
		}
	}

	vkDestroyCommandPool(s->dev, pool, NULL);
	vkDestroyBuffer(s->dev, buf, NULL);
	vkFreeMemory(s->dev, bmem, NULL);
	vkFreeMemory(s->dev, mem, NULL); /* also closes the imported fd */
	vkDestroyImage(s->dev, image, NULL);
	return r == VK_SUCCESS && r3 == VK_SUCCESS && ok_pattern;
}


/* Multi-plane import: the shape the engine would actually use for
 * NV12/P010 (one VkImage over the whole CUDA allocation, two plane views),
 * mirroring the D3D11VA multi-planar import in quercus-core. */
static int vk_import_multiplane_test(struct vk_state *s, int fd, uint32_t width,
                                     uint32_t height, VkFormat fmt,
                                     const char *label, uint32_t chroma_height,
                                     VkExternalMemoryHandleTypeFlagBits ht)
{
	if (fd < 0 || !s->dev)
		return 0;
	uint32_t bytes = (fmt == VK_FORMAT_G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16 ||
	                  fmt == VK_FORMAT_G10X6_B10X6R10X6_2PLANE_422_UNORM_3PACK16)
	                 ? 2u : 1u;
	uint32_t luma_pitch = width * bytes;
	uint32_t chroma_pitch = width * bytes;
	uint64_t chroma_off = (uint64_t)luma_pitch * height;
	VkSubresourceLayout layouts[2] = {
		{ .offset = 0, .size = 0, .rowPitch = luma_pitch, .arrayPitch = 0,
		  .depthPitch = 0 },
		{ .offset = chroma_off, .size = 0, .rowPitch = chroma_pitch,
		  .arrayPitch = 0, .depthPitch = 0 },
	};
	(void)chroma_height;

	VkExternalMemoryImageCreateInfo ext = { 0 };
	ext.sType = VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_IMAGE_CREATE_INFO;
	ext.handleTypes = ht;
	VkImageDrmFormatModifierExplicitCreateInfoEXT mod = { 0 };
	mod.sType = VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_EXPLICIT_CREATE_INFO_EXT;
	mod.drmFormatModifier = 0;
	mod.drmFormatModifierPlaneCount = 2;
	mod.pPlaneLayouts = layouts;
	mod.pNext = &ext;

	VkImageCreateInfo ici = { 0 };
	ici.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
	ici.imageType = VK_IMAGE_TYPE_2D;
	ici.format = fmt;
	ici.extent.width = width;
	ici.extent.height = height;
	ici.extent.depth = 1;
	ici.mipLevels = 1;
	ici.arrayLayers = 1;
	ici.samples = VK_SAMPLE_COUNT_1_BIT;
	ici.tiling = VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT;
	ici.usage = VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
	ici.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
	ici.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
	ici.pNext = &mod;

	VkImage image = VK_NULL_HANDLE;
	VkResult r = vkCreateImage(s->dev, &ici, NULL, &image);
	printf("  [%s] vkCreateImage(multiplane) -> %s\n", label, vkres_str(r));
	if (r != VK_SUCCESS)
		return 0;

	VkMemoryRequirements req;
	vkGetImageMemoryRequirements(s->dev, image, &req);
	int dupfd = fcntl(fd, F_DUPFD_CLOEXEC, 3);
	uint32_t mt = find_memory_type(s->phys, req.memoryTypeBits,
	                               VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT);
	if (mt == UINT32_MAX)
		mt = find_memory_type(s->phys, req.memoryTypeBits, 0);
	VkMemoryDedicatedAllocateInfo ded = { 0 };
	ded.sType = VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO;
	ded.image = image;
	VkImportMemoryFdInfoKHR imp = { 0 };
	imp.sType = VK_STRUCTURE_TYPE_IMPORT_MEMORY_FD_INFO_KHR;
	imp.handleType = ht;
	imp.fd = dupfd;
	imp.pNext = &ded;
	VkMemoryAllocateInfo mai = { 0 };
	mai.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
	mai.allocationSize = req.size;
	mai.memoryTypeIndex = mt;
	mai.pNext = &imp;
	VkDeviceMemory mem = VK_NULL_HANDLE;
	VkResult r2 = vkAllocateMemory(s->dev, &mai, NULL, &mem);
	printf("  [%s] vkAllocateMemory(import DMA_BUF, size=%llu) -> %s\n", label,
	       (unsigned long long)req.size, vkres_str(r2));
	if (r2 != VK_SUCCESS) {
		if (dupfd >= 0)
			close(dupfd);
		vkDestroyImage(s->dev, image, NULL);
		return 0;
	}
	r = vkBindImageMemory(s->dev, image, mem, 0);
	printf("  [%s] vkBindImageMemory -> %s\n", label, vkres_str(r));
	if (r != VK_SUCCESS) {
		vkFreeMemory(s->dev, mem, NULL);
		vkDestroyImage(s->dev, image, NULL);
		return 0;
	}

	/* two plane views */
	for (uint32_t plane = 0; plane < 2; plane++) {
		VkImageViewCreateInfo vci = { 0 };
		vci.sType = VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO;
		vci.image = image;
		vci.viewType = VK_IMAGE_VIEW_TYPE_2D;
		vci.format = fmt;
		vci.subresourceRange.aspectMask =
			plane == 0 ? VK_IMAGE_ASPECT_PLANE_0_BIT
			           : VK_IMAGE_ASPECT_PLANE_1_BIT;
		vci.subresourceRange.levelCount = 1;
		vci.subresourceRange.layerCount = 1;
		VkImageView view = VK_NULL_HANDLE;
		VkResult rv = vkCreateImageView(s->dev, &vci, NULL, &view);
		printf("  [%s] vkCreateImageView(plane %u) -> %s\n", label, plane,
		       vkres_str(rv));
		if (rv == VK_SUCCESS)
			vkDestroyImageView(s->dev, view, NULL);
	}

	/* copy plane 0 to a host-visible buffer */
	VkBufferCreateInfo bci = { 0 };
	bci.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO;
	bci.size = 64;
	bci.usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT;
	VkBuffer buf = VK_NULL_HANDLE;
	vkCreateBuffer(s->dev, &bci, NULL, &buf);
	VkMemoryRequirements breq;
	vkGetBufferMemoryRequirements(s->dev, buf, &breq);
	uint32_t bmt = find_memory_type(s->phys, breq.memoryTypeBits,
	                                VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
	                                VK_MEMORY_PROPERTY_HOST_COHERENT_BIT);
	VkMemoryAllocateInfo bmai = { 0 };
	bmai.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO;
	bmai.allocationSize = breq.size;
	bmai.memoryTypeIndex = bmt;
	VkDeviceMemory bmem = VK_NULL_HANDLE;
	vkAllocateMemory(s->dev, &bmai, NULL, &bmem);
	vkBindBufferMemory(s->dev, buf, bmem, 0);

	VkCommandPoolCreateInfo pci = { 0 };
	pci.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO;
	pci.queueFamilyIndex = s->queue_family;
	VkCommandPool pool = VK_NULL_HANDLE;
	vkCreateCommandPool(s->dev, &pci, NULL, &pool);
	VkCommandBufferAllocateInfo cai = { 0 };
	cai.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO;
	cai.commandPool = pool;
	cai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
	cai.commandBufferCount = 1;
	VkCommandBuffer cmd;
	vkAllocateCommandBuffers(s->dev, &cai, &cmd);
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
	b.subresourceRange.aspectMask = VK_IMAGE_ASPECT_PLANE_0_BIT;
	b.subresourceRange.levelCount = 1;
	b.subresourceRange.layerCount = 1;
	b.dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
	vkCmdPipelineBarrier(cmd, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
	                     VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1,
	                     &b);
	VkBufferImageCopy region = { 0 };
	region.imageSubresource.aspectMask = VK_IMAGE_ASPECT_PLANE_0_BIT;
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
	VkResult r3 = vkQueueSubmit(s->queue, 1, &si, VK_NULL_HANDLE);
	if (r3 == VK_SUCCESS)
		r3 = vkQueueWaitIdle(s->queue);
	printf("  [%s] plane0 -> buffer readback -> %s\n", label, vkres_str(r3));
	if (r3 == VK_SUCCESS) {
		void *mapped = NULL;
		if (vkMapMemory(s->dev, bmem, 0, 64, 0, &mapped) == VK_SUCCESS) {
			printf("        first bytes:");
			for (int i = 0; i < 8; i++)
				printf(" %02x", ((unsigned char *)mapped)[i]);
			printf("\n");
			vkUnmapMemory(s->dev, bmem);
		}
	}
	vkDestroyCommandPool(s->dev, pool, NULL);
	vkDestroyBuffer(s->dev, buf, NULL);
	vkFreeMemory(s->dev, bmem, NULL);
	vkFreeMemory(s->dev, mem, NULL);
	vkDestroyImage(s->dev, image, NULL);
	return r3 == VK_SUCCESS;
}


/* ------------------------------------------------------------------ */
/* Cross-process check: pass the exported fd to a freshly exec'ed      */
/* child, which imports it on its own Vulkan device and verifies 0xAB. */
/* (Decisive for the engine's quercus-worker process pool.)            */
/* ------------------------------------------------------------------ */

static int send_fd(int sock, int fd)
{
	struct msghdr msg;
	struct iovec io;
	char buf[1] = { 0 };
	char cmsgbuf[CMSG_SPACE(sizeof(int))];
	memset(&msg, 0, sizeof(msg));
	memset(cmsgbuf, 0, sizeof(cmsgbuf));
	io.iov_base = buf;
	io.iov_len = 1;
	msg.msg_iov = &io;
	msg.msg_iovlen = 1;
	msg.msg_control = cmsgbuf;
	msg.msg_controllen = sizeof(cmsgbuf);
	struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
	cmsg->cmsg_level = SOL_SOCKET;
	cmsg->cmsg_type = SCM_RIGHTS;
	cmsg->cmsg_len = CMSG_LEN(sizeof(int));
	memcpy(CMSG_DATA(cmsg), &fd, sizeof(int));
	return sendmsg(sock, &msg, 0) == 1 ? 0 : -1;
}

static int recv_fd(int sock)
{
	struct msghdr msg;
	struct iovec io;
	char buf[1];
	char cmsgbuf[CMSG_SPACE(sizeof(int))];
	memset(&msg, 0, sizeof(msg));
	memset(cmsgbuf, 0, sizeof(cmsgbuf));
	io.iov_base = buf;
	io.iov_len = 1;
	msg.msg_iov = &io;
	msg.msg_iovlen = 1;
	msg.msg_control = cmsgbuf;
	msg.msg_controllen = sizeof(cmsgbuf);
	if (recvmsg(sock, &msg, 0) != 1)
		return -1;
	struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
	if (!cmsg || cmsg->cmsg_type != SCM_RIGHTS)
		return -1;
	int fd = -1;
	memcpy(&fd, CMSG_DATA(cmsg), sizeof(int));
	return fd;
}

static int cross_process_import(int fd, uint32_t width, uint32_t height,
                                uint32_t pitch)
{
	int sv[2];
	if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
		printf("[xproc] socketpair failed: %s\n", strerror(errno));
		return -1;
	}
	pid_t pid = fork();
	if (pid < 0) {
		printf("[xproc] fork failed: %s\n", strerror(errno));
		return -1;
	}
	if (pid == 0) {
		close(sv[0]);
		int got = recv_fd(sv[1]);
		close(sv[1]);
		if (got < 0)
			_exit(3);
		char fdbuf[32];
		snprintf(fdbuf, sizeof(fdbuf), "%d", got);
		execl("/proc/self/exe", "cuda_vulkan_probe", "--fd-child", fdbuf,
		      (char *)NULL);
		_exit(127);
	}
	close(sv[1]);
	int sr = send_fd(sv[0], fd);
	printf("[xproc] sent fd=%d to child pid=%d (send=%s), width=%u height=%u "
	       "pitch=%u\n", fd, (int)pid, sr == 0 ? "ok" : "fail", width, height,
	       pitch);
	close(sv[0]);
	int st = 0;
	waitpid(pid, &st, 0);
	if (WIFEXITED(st))
		return WEXITSTATUS(st) == 0;
	return -1;
}

/* ------------------------------------------------------------------ */
/* main                                                                */
/* ------------------------------------------------------------------ */

int main(int argc, char **argv)
{
	if (argc == 3 && strcmp(argv[1], "--fd-child") == 0) {
		int fd = atoi(argv[2]);
		struct vk_state child_vk;
		printf("[child] new process, importing fd=%d on its own Vulkan device\n",
		       fd);
		if (!vk_init(&child_vk)) {
			fprintf(stderr, "[child] no Vulkan device\n");
			return 2;
		}
		int ok = vk_import_test(
			&child_vk, fd, 1920, 1080, 1920,
			"cross-process OPAQUE_FD + LINEAR", VK_IMAGE_TILING_LINEAR,
			VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT);
		if (child_vk.dev)
			vkDestroyDevice(child_vk.dev, NULL);
		if (child_vk.instance)
			vkDestroyInstance(child_vk.instance, NULL);
		return ok ? 0 : 1;
	}

	printf("Quercus CUDA->Vulkan zero-copy spike probe\n");
	printf("(build: cc -O2 -o cuda_vulkan_probe cuda_vulkan_probe.c -ldl -lvulkan)\n");

	struct vk_state vk;
	int have_vk = vk_init(&vk);
	if (!have_vk)
		printf("[vulkan] no usable Vulkan device (headless container without "
		       "/dev/dri or /dev/nvidia*?) - capability sections below will be "
		       "skipped\n");

	if (have_vk)
		query_video(&vk);

	struct cuda_api cu;
	struct cuda_probe cp;
	int have_cuda = cuda_load(&cu) && cuda_probe_run(&cu, &cp);

	if (have_vk)
		check_linear_import_caps(&vk);

	struct cuda_vmm_result vmm;
	int have_vmm = have_cuda ? cuda_route_b(&cu, &cp, &vmm) : 0;

	if (have_cuda && have_vk && cp.fd >= 0) {
		printf("\n== CUDA dma-buf -> Vulkan image import ==\n");
		printf("[probe] exported fd=%d, %ux%u, assumed pitch=%u\n", cp.fd,
		       cp.width, cp.height, cp.width);
		vk_import_test(&vk, cp.fd, cp.width, cp.height, cp.width,
		               "DMA_BUF + DRM_MODIFIER(LINEAR)",
		               VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
		               VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT);
		vk_import_test(&vk, cp.fd, cp.width, cp.height, cp.width,
		               "DMA_BUF + LINEAR tiling", VK_IMAGE_TILING_LINEAR,
		               VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT);
		vk_import_test(&vk, cp.fd, cp.width, cp.height, cp.width,
		               "OPAQUE_FD + LINEAR tiling", VK_IMAGE_TILING_LINEAR,
		               VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT);
		vk_import_multiplane_test(&vk, cp.fd, cp.width, cp.height,
		                          VK_FORMAT_G8_B8R8_2PLANE_420_UNORM,
		                          "NV12 2-plane 8bit (unaliased pitch)",
		                          cp.height / 2,
		                          VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT);
		vk_import_multiplane_test(&vk, cp.fd, cp.width, cp.height,
		                          VK_FORMAT_G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
		                          "P010 2-plane 10bit (unaliased pitch)",
		                          cp.height / 2,
		                          VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT);
		vk_import_multiplane_test(&vk, cp.fd, cp.width, cp.height,
		                          VK_FORMAT_G10X6_B10X6R10X6_2PLANE_422_UNORM_3PACK16,
		                          "P210 2-plane 422 10bit (unaliased pitch)",
		                          cp.height,
		                          VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT);
	} else if (have_cuda && cp.fd < 0) {
		printf("\n== CUDA dma-buf -> Vulkan image import ==\n");
		printf("  SKIP: no dma-buf fd could be exported (see CUDA section)\n");
	}

	if (have_vk && have_vmm) {
		printf("\n== route B: CUDA VMM fd -> Vulkan import (expect byte 0xAB) ==\n");
		if (vmm.exported_fd >= 0 || vmm.range_fd >= 0)
			printf("[routeB] exported fd kind=%s, VA-range fd kind=%s\n",
			       vmm.exported_fd >= 0 ? vmm.export_kind : "(none)",
			       vmm.range_fd >= 0 ? vmm.range_kind : "(none)");
		if (vmm.exported_fd >= 0) {
			vk_import_test(&vk, vmm.exported_fd, cp.width, cp.height, cp.width,
			               "VMM export fd -> OPAQUE_FD + LINEAR",
			               VK_IMAGE_TILING_LINEAR,
			               VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT);
			vk_import_multiplane_test(
				&vk, vmm.exported_fd, cp.width, cp.height,
				VK_FORMAT_G10X6_B10X6R10X6_2PLANE_420_UNORM_3PACK16,
				"VMM export fd -> OPAQUE_FD + P010 2-plane", cp.height / 2,
				VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT);
			vk_import_test(&vk, vmm.exported_fd, cp.width, cp.height, cp.width,
			               "VMM export fd -> DMA_BUF + DRM_MODIFIER(LINEAR)",
			               VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
			               VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT);
		}
		if (vmm.range_fd >= 0) {
			vk_import_test(&vk, vmm.range_fd, cp.width, cp.height, cp.width,
			               "VMM VA range -> DMA_BUF + DRM_MODIFIER(LINEAR)",
			               VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT,
			               VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT);
			vk_import_test(&vk, vmm.range_fd, cp.width, cp.height, cp.width,
			               "VMM VA range -> DMA_BUF + LINEAR",
			               VK_IMAGE_TILING_LINEAR,
			               VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT);
		}
		if (vmm.exported_fd < 0 && vmm.range_fd < 0)
			printf("[routeB] no fd obtained; nothing to import\n");
		if (vmm.exported_fd >= 0) {
			printf("\n== cross-process OPAQUE_FD import (SCM_RIGHTS + exec) ==\n");
			int x = cross_process_import(vmm.exported_fd, cp.width, cp.height,
			                             cp.width);
			printf("[routeB] cross-process OPAQUE_FD import: %s\n",
			       x > 0 ? "GO (same fd imports in another process)"
			             : "FAIL (fd is process-local)");
		}
	}

	/* summary */
	printf("\n== summary ==\n");
	printf("  vulkan device            : %s\n",
	       have_vk ? vk.props.deviceName : "none");
	printf("  cuda device              : %s\n",
	       have_cuda ? cp.name : "none");
	if (have_cuda)
		printf("  cuda dma-buf supported   : %s\n",
		       cp.attr_dma_buf ? "YES" : "NO");
	printf("  dma_buf export fd        : %s\n",
	       (have_cuda && cp.fd >= 0) ? "YES" : "NO");
	if (have_cuda)
		printf("  VMM supported            : %s\n",
		       vmm.vmm_supported ? "YES" : "NO");
	if (have_cuda)
		printf("  POSIX_FD handle supported: %s\n",
		       vmm.posix_fd_supported ? "YES" : "NO");
	if (have_vmm)
		printf("  route B export fd        : %s\n",
		       vmm.exported_fd >= 0 ? "YES (POSIX_FD)" : "NO");
	if (have_vmm)
		printf("  route B VA range fd      : %s\n",
		       vmm.range_fd >= 0 ? "YES (DMA_BUF)" : "NO");
	if (have_vk)
		printf("  dma_buf import extension : %s\n",
		       vk.has_ext_dma_buf ? "YES" : "NO");
	printf("\nInterpretation:\n");
	printf("  - Route A (cuMemAlloc frame -> DMA-BUF) = GO only if "
	       "route-A export fd is YES and its import test reports VK_SUCCESS.\n");
	printf("  - Route B (VMM POSIX_FD export -> Vulkan OPAQUE_FD/DMA_BUF) = GO "
	       "only if 'route B export fd' or 'route B VA range fd' is YES and the "
	       "corresponding import test reports VK_SUCCESS with byte 0xAB.\n");
	printf("  - Vulkan Video sections answer the h264_vulkan/hevc_vulkan "
	       "10-bit 4:2:2 question independently of FFmpeg.\n");

	/* cleanup (best effort) */
	if (have_vmm)
		cuda_vmm_cleanup(&cu, &vmm);
	if (have_cuda) {
		if (cu.ctx_push && cp.ctx)
			cu.ctx_push(cp.ctx);
		if (cp.ptr && cu.mem_free)
			cu.mem_free(cp.ptr);
		if (cp.fd >= 0)
			close(cp.fd);
		if (cu.ctx_pop && cp.ctx) {
			CUcontext dummy = NULL;
			cu.ctx_pop(&dummy);
		}
		if (cu.ctx_destroy && cp.ctx)
			cu.ctx_destroy(cp.ctx);
		if (cu.dl)
			dlclose(cu.dl);
	}
	if (have_vk) {
		if (vk.dev)
			vkDestroyDevice(vk.dev, NULL);
		if (vk.instance)
			vkDestroyInstance(vk.instance, NULL);
	}
	return 0;
}
