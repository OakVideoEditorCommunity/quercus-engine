/*
 * Quercus Video Editor - Non-Linear Video Editor
 * Copyright (C) 2026 Quercus Team
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 */

/*
 * Minimal test plugin (M11 §2.4 deliverable; build.rs compiles it to a
 * shared library, assembled at run time into quercus-test-plugin.ofx.bundle
 * for the host to scan).
 *
 * filter context:
 *  - parameters: gain (Double, default 0)
 *  - clips: Source (input, RGBA), Output
 *  - describe/describeInContext create parameters and clips;
 *  - getClipPreferences: whole chain F32+RGBA, frame rate 24;
 *  - render: fill the output image with constant 0.5 (RGBA float), alpha=1;
 *  - getRoD: project size; getRoI: return region unchanged;
 *  - isIdentity: unset → not identity;
 *  - begin/endSequenceRender: log only (status OK).
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdarg.h>
#ifdef _WIN32
#include <windows.h>
#else
#include <unistd.h>
#endif

/* On Windows the plugin DLL carries its own CRT environment block: the
 * host's Rust-side std::env::set_var goes through SetEnvironmentVariableW and
 * only updates the Win32 process environment, which the CRT `_environ` that
 * getenv reads inside the plugin cannot see. Marker paths are read uniformly
 * from the Win32 process environment (plain getenv on POSIX). */
static const char *marker_env(const char *name)
{
#ifdef _WIN32
    static char buf[32768];
    DWORD n = GetEnvironmentVariableA(name, buf, (DWORD)sizeof buf);
    if (n == 0 || n >= sizeof buf)
        return NULL;
    return buf;
#else
    return getenv(name);
#endif
}

#include "ofxCore.h"
#include "ofxColour.h"
#include "ofxDrawSuite.h"
#include "ofxGPURender.h"
#include "ofxImageEffect.h"
#include "ofxInteract.h"
#include "ofxKeySyms.h"
#include "ofxMessage.h"
#include "ofxParam.h"
#include "ofxProgress.h"
#include "ofxProperty.h"

/* Real drawing for the GL end-to-end test needs GL commands (only macOS has
 * a real CGL bridge; the Linux/Windows GL bridges are stubs, so the drawing
 * code is excluded behind __APPLE__). */
#ifdef __APPLE__
#include <OpenGL/gl.h>
#elif defined(_WIN32)
#include <GL/gl.h>
#else
#include <GL/gl.h>
#endif

/* kOfxImageEffectGLFormatRGBA is a stub in the vendored ofxOpenGLRender.h
 * and not included (OpenFX 1.4 canonical name), so define it per spec.
 * kOfxImageEffectPropIsIdentity likewise (the vendored ofxImageEffect.h only
 * documents that property, without providing the macro). */
#define GL_FORMAT_RGBA "OfxImageEffectGLFormatRGBA"
#define kOfxImageEffectPropIsIdentity "OfxImageEffectPropIsIdentity"

static OfxHost *g_host = NULL;
static const OfxPropertySuiteV1 *g_propSuite = NULL;
static const OfxImageEffectSuiteV1 *g_imageEffectSuite = NULL;
static const OfxParameterSuiteV1 *g_paramSuite = NULL;
static const OfxMessageSuiteV1 *g_messageSuite = NULL;
static const OfxProgressSuiteV1 *g_progressSuite = NULL;
static const OfxImageEffectOpenGLRenderSuiteV1 *g_glSuite = NULL;
static const OfxDrawSuiteV1 *g_drawSuite = NULL;
static const OfxInteractSuiteV1 *g_interactSuite = NULL;

/* Host extensions (task contract; action and property names absent from the
 * vendored ofxInteract.h). kOfxInteractPropViewportSize uses the OFX 1.3
 * canonical name "OfxInteractPropViewport" (removed from the vendored 1.5
 * headers); BackgroundImage and NewInteract/Idle are this host's extensions. */
#ifndef kOfxActionNewInteract
#define kOfxActionNewInteract "OfxActionNewInteract"
#endif
#ifndef kOfxInteractActionIdle
#define kOfxInteractActionIdle "OfxInteractActionIdle"
#endif
#ifndef kOfxInteractPropViewportSize
#define kOfxInteractPropViewportSize "OfxInteractPropViewport"
#endif
#ifndef kOfxInteractPropBackgroundImage
#define kOfxInteractPropBackgroundImage "OfxInteractPropBackgroundImage"
#endif

/* Push button instanceChanged call count (after pressing the button the host
 * should route kOfxActionInstanceChanged/UserEdited here). */
static int g_button_instance_changed = 0;

/* ---------- suite convenience wrappers ---------- */

/* Record one push button instanceChanged: bump the count and append a line to
 * the file named by QUERCUS_TEST_PLUGIN_INSTANCECHANGED_MARKER (for host test
 * assertions; silent when the env var is unset). */
static void record_button_instance_changed(void)
{
    g_button_instance_changed++;
    const char *marker = marker_env("QUERCUS_TEST_PLUGIN_INSTANCECHANGED_MARKER");
    if (!marker)
        return;
    FILE *f = fopen(marker, "a");
    if (!f)
        return;
    fprintf(f, "button instanceChanged count=%d\n", g_button_instance_changed);
    fclose(f);
}

static OfxStatus propSetString(OfxPropertySetHandle h, const char *name, int index, const char *v)
{
    return g_propSuite->propSetString(h, name, index, v);
}

static OfxStatus propSetStringN(OfxPropertySetHandle h, const char *name, int count, const char **v)
{
    return g_propSuite->propSetStringN(h, name, count, v);
}

static OfxStatus propSetPointer(OfxPropertySetHandle h, const char *name, int index, void *v)
{
    return g_propSuite->propSetPointer(h, name, index, v);
}

static OfxStatus propSetDoubleN(OfxPropertySetHandle h, const char *name, int count, const double *v)
{
    return g_propSuite->propSetDoubleN(h, name, count, v);
}

static OfxStatus propGetDouble(OfxPropertySetHandle h, const char *name, int index, double *v)
{
    return g_propSuite->propGetDouble(h, name, index, v);
}

static OfxStatus propGetInt(OfxPropertySetHandle h, const char *name, int index, int *v)
{
    return g_propSuite->propGetInt(h, name, index, v);
}

static OfxStatus propGetIntN(OfxPropertySetHandle h, const char *name, int count, int *v)
{
    return g_propSuite->propGetIntN(h, name, count, v);
}

static OfxStatus propGetPointer(OfxPropertySetHandle h, const char *name, int index, void **v)
{
    return g_propSuite->propGetPointer(h, name, index, v);
}

/* Negotiation-time per-clip property names: prefix_clip */
static void setClipPref(OfxPropertySetHandle out, const char *prefix, const char *clip, const char *value)
{
    char name[256];
    snprintf(name, sizeof(name), "%s_%s", prefix, clip);
    propSetString(out, name, 0, value);
}

/* ---------- setHost (mandatory first call) ---------- */

static void setHost(OfxHost *host)
{
    g_host = host;
    if (!host)
        return;
    g_propSuite = (const OfxPropertySuiteV1 *)host->fetchSuite(host->host, kOfxPropertySuite, 1);
    g_imageEffectSuite =
        (const OfxImageEffectSuiteV1 *)host->fetchSuite(host->host, kOfxImageEffectSuite, 1);
    g_paramSuite = (const OfxParameterSuiteV1 *)host->fetchSuite(host->host, kOfxParameterSuite, 1);
    g_messageSuite = (const OfxMessageSuiteV1 *)host->fetchSuite(host->host, kOfxMessageSuite, 1);
    g_progressSuite = (const OfxProgressSuiteV1 *)host->fetchSuite(host->host, kOfxProgressSuite, 1);
    g_glSuite = (const OfxImageEffectOpenGLRenderSuiteV1 *)host->fetchSuite(
        host->host, kOfxOpenGLRenderSuite, 1);
    g_drawSuite = (const OfxDrawSuiteV1 *)host->fetchSuite(host->host, kOfxDrawSuite, 1);
    g_interactSuite = (const OfxInteractSuiteV1 *)host->fetchSuite(host->host, kOfxInteractSuite, 1);
}

/* ---------- action implementations ---------- */

static OfxStatus actionDescribe(const void *handle, int is_gl)
{
    const char *comps[] = { kOfxImageComponentRGBA, kOfxImageComponentRGB, kOfxImageComponentAlpha };

    propSetString(handle, kOfxPropLabel, 0, "Quercus Test Plugin");
    propSetStringN(handle, kOfxImageEffectPropSupportedContexts, 1,
                   &(const char *){ kOfxImageEffectContextFilter });

    /* ofxColour (M11 §4): declare OCIO colour management capability and the
     * available configs. */
    propSetString(handle, kOfxImageEffectPropColourManagementStyle, 0,
                  kOfxImageEffectColourManagementOCIO);
    {
        const char *configs[] = { "ofx-native-v1.5_aces-v1.3_ocio-v2.3" };
        propSetStringN(handle, kOfxImageEffectPropColourManagementAvailableConfigs, 1, configs);
    }

    /* GL capability (M11 §4): the GL variant declares "true" + supported
     * bit depths (F32). The CPU variant keeps the default "false". */
    if (is_gl) {
        propSetString(handle, kOfxImageEffectPropOpenGLRenderSupported, 0, "true");
        {
            const char *depths[] = { kOfxBitDepthFloat };
            propSetStringN(handle, kOfxOpenGLPropPixelDepth, 1, depths);
        }
    }

    /* Parameter family: gain (Double, with display min/max), mode (Choice,
     * two options), debug (Double, secret), label (String). */
    OfxPropertySetHandle paramProps = NULL;
    OfxStatus st = g_paramSuite->paramDefine((OfxParamSetHandle)handle, kOfxParamTypeDouble, "gain", &paramProps);
    if (st != kOfxStatOK)
        return st;
    propSetString(paramProps, kOfxPropLabel, 0, "Gain");
    {
        const double min = -2.0, max = 2.0;
        g_propSuite->propSetDouble(paramProps, kOfxParamPropDisplayMin, 0, min);
        g_propSuite->propSetDouble(paramProps, kOfxParamPropDisplayMax, 0, max);
    }

    st = g_paramSuite->paramDefine((OfxParamSetHandle)handle, kOfxParamTypeChoice, "mode", &paramProps);
    if (st != kOfxStatOK)
        return st;
    propSetString(paramProps, kOfxPropLabel, 0, "Mode");
    {
        const char *options[] = { "Fast", "High" };
        g_propSuite->propSetStringN(paramProps, kOfxParamPropChoiceOption, 2, options);
    }

    st = g_paramSuite->paramDefine((OfxParamSetHandle)handle, kOfxParamTypeDouble, "debug", &paramProps);
    if (st != kOfxStatOK)
        return st;
    g_propSuite->propSetInt(paramProps, kOfxParamPropSecret, 0, 1);

    st = g_paramSuite->paramDefine((OfxParamSetHandle)handle, kOfxParamTypeString, "label", &paramProps);
    if (st != kOfxStatOK)
        return st;
    propSetString(paramProps, kOfxPropLabel, 0, "Label");

    /* push button: no value; a press notifies the plugin via instanceChanged
     * (UserEdited) (ofxCore.h kOfxActionInstanceChanged). */
    st = g_paramSuite->paramDefine((OfxParamSetHandle)handle, kOfxParamTypePushButton, "button", &paramProps);
    if (st != kOfxStatOK)
        return st;
    propSetString(paramProps, kOfxPropLabel, 0, "Button");

    /* clips: Source + Output. */
    OfxPropertySetHandle clipProps = NULL;
    st = g_imageEffectSuite->clipDefine((OfxImageEffectHandle)handle, "Source", &clipProps);
    if (st != kOfxStatOK)
        return st;
    propSetStringN(clipProps, kOfxImageEffectPropSupportedComponents, 3, comps);
    propSetString(clipProps, kOfxPropLabel, 0, "Source");

    st = g_imageEffectSuite->clipDefine((OfxImageEffectHandle)handle, "Output", &clipProps);
    if (st != kOfxStatOK)
        return st;
    propSetStringN(clipProps, kOfxImageEffectPropSupportedComponents, 3, comps);
    return kOfxStatOK;
}

static OfxStatus actionGetClipPreferences(OfxPropertySetHandle outArgs)
{
    setClipPref(outArgs, "OfxImageClipPropComponents", "Source", kOfxImageComponentRGBA);
    setClipPref(outArgs, "OfxImageClipPropDepth", "Source", kOfxBitDepthFloat);
    setClipPref(outArgs, "OfxImageClipPropComponents", "Output", kOfxImageComponentRGBA);
    setClipPref(outArgs, "OfxImageClipPropDepth", "Output", kOfxBitDepthFloat);
    g_propSuite->propSetDouble(outArgs, kOfxImageEffectPropFrameRate, 0, 24.0);
    g_propSuite->propSetString(outArgs, kOfxImageClipPropFieldOrder, 0, kOfxImageFieldNone);
    return kOfxStatOK;
}

static OfxStatus actionGetRoD(OfxPropertySetHandle outArgs)
{
    const double rod[4] = { 0.0, 0.0, 1920.0, 1080.0 };
    propSetDoubleN(outArgs, kOfxImageEffectPropRegionOfDefinition, 4, rod);
    return kOfxStatOK;
}

static OfxStatus actionGetRoI(OfxImageEffectHandle inst, OfxPropertySetHandle inArgs,
                              OfxPropertySetHandle outArgs)
{
    /* Write the region back for each input clip (here just returning the
     * Output window unchanged is enough). */
    OfxImageClipHandle clip = NULL;
    OfxPropertySetHandle clipProps = NULL;
    OfxStatus st = g_imageEffectSuite->clipGetHandle(inst, "Source", &clip, &clipProps);
    if (st != kOfxStatOK)
        return st;
    double region[4] = { 0.0, 0.0, 0.0, 0.0 };
    st = g_propSuite->propGetDoubleN(inArgs, kOfxImageEffectPropRegionOfInterest, 4, region);
    if (st != kOfxStatOK)
        return st;
    char name[256];
    snprintf(name, sizeof(name), "OfxImageEffectPropRegionOfInterest_%s", "Source");
    st = propSetDoubleN(outArgs, name, 4, region);
    fprintf(stderr, "DBG getRoI read=(%g,%g,%g,%g) set st=%d\n", region[0], region[1], region[2], region[3], st);
    if (st != kOfxStatOK)
        return st;
    return kOfxStatOK;
}

static OfxStatus actionRender(OfxImageEffectHandle inst, OfxPropertySetHandle inArgs)
{
    double time = 0.0;
    propGetDouble(inArgs, kOfxPropTime, 0, &time);

    OfxImageClipHandle clip = NULL;
    OfxPropertySetHandle clipProps = NULL;
    OfxStatus st = g_imageEffectSuite->clipGetHandle(inst, "Output", &clip, &clipProps);
    if (st != kOfxStatOK)
        return st;

    OfxPropertySetHandle image = NULL;
    st = g_imageEffectSuite->clipGetImage(clip, time, NULL, &image);
    if (st != kOfxStatOK)
        return st;

    void *data = NULL;
    int rowBytes = 0;
    int bounds[4] = { 0, 0, 0, 0 };
    propGetPointer(image, kOfxImagePropData, 0, &data);
    propGetInt(image, kOfxImagePropRowBytes, 0, &rowBytes);
    propGetIntN(image, kOfxImagePropBounds, 4, bounds);

    /* Progress: Start → Update(0.5) (abort if cancelled) → End. */
    if (g_progressSuite) {
        g_progressSuite->progressStart((OfxImageEffectHandle)inst, "render");
        OfxStatus ps = g_progressSuite->progressUpdate((OfxImageEffectHandle)inst, 0.5);
        if (ps != kOfxStatOK) {
            g_progressSuite->progressEnd((OfxImageEffectHandle)inst);
            return kOfxStatFailed;
        }
    }

    int w = bounds[2] - bounds[0];
    int h = bounds[3] - bounds[1];
    if (data && w > 0 && h > 0) {
        /* Constant 0.5 (RGBA float); alpha=1. */
        for (int y = 0; y < h; y++) {
            float *row = (float *)((char *)data + (size_t)y * (size_t)rowBytes);
            for (int x = 0; x < w; x++) {
                row[x * 4 + 0] = 0.5f;
                row[x * 4 + 1] = 0.5f;
                row[x * 4 + 2] = 0.5f;
                row[x * 4 + 3] = 1.0f;
            }
        }
    }

    g_imageEffectSuite->clipReleaseImage(image);
    if (g_progressSuite) {
        g_progressSuite->progressEnd((OfxImageEffectHandle)inst);
    }
    return kOfxStatOK;
}

/* ---------- slow variant (M3): cancellation e2e test ---------- */

/* Sleep between progressUpdate calls (the host sets cancellation during the
 * render; the next progressUpdate returns non-OK and the plugin aborts). */
static void slow_sleep_ms(int ms)
{
#ifdef _WIN32
    Sleep((DWORD)ms);
#else
    usleep((useconds_t)ms * 1000);
#endif
}

/* 100 progressUpdate calls x 20ms (~2s): fill the output with constant 0.25.
 * Any progressUpdate returning non-OK (host cancellation) → kOfxStatFailed. */
static OfxStatus actionRenderSlow(OfxImageEffectHandle inst, OfxPropertySetHandle inArgs)
{
    double time = 0.0;
    propGetDouble(inArgs, kOfxPropTime, 0, &time);

    OfxImageClipHandle clip = NULL;
    OfxPropertySetHandle clipProps = NULL;
    OfxStatus st = g_imageEffectSuite->clipGetHandle(inst, "Output", &clip, &clipProps);
    if (st != kOfxStatOK)
        return st;

    OfxPropertySetHandle image = NULL;
    st = g_imageEffectSuite->clipGetImage(clip, time, NULL, &image);
    if (st != kOfxStatOK)
        return st;

    void *data = NULL;
    int rowBytes = 0;
    int bounds[4] = { 0, 0, 0, 0 };
    propGetPointer(image, kOfxImagePropData, 0, &data);
    propGetInt(image, kOfxImagePropRowBytes, 0, &rowBytes);
    propGetIntN(image, kOfxImagePropBounds, 4, bounds);

    if (g_progressSuite) {
        g_progressSuite->progressStart((OfxImageEffectHandle)inst, "slow-render");
        for (int i = 0; i < 100; i++) {
            OfxStatus ps = g_progressSuite->progressUpdate((OfxImageEffectHandle)inst,
                                                           (double)(i + 1) / 100.0);
            if (ps != kOfxStatOK) {
                g_progressSuite->progressEnd((OfxImageEffectHandle)inst);
                return kOfxStatFailed;
            }
            slow_sleep_ms(20);
        }
    }

    int w = bounds[2] - bounds[0];
    int h = bounds[3] - bounds[1];
    if (data && w > 0 && h > 0) {
        for (int y = 0; y < h; y++) {
            float *row = (float *)((char *)data + (size_t)y * (size_t)rowBytes);
            for (int x = 0; x < w; x++) {
                row[x * 4 + 0] = 0.25f;
                row[x * 4 + 1] = 0.25f;
                row[x * 4 + 2] = 0.25f;
                row[x * 4 + 3] = 1.0f;
            }
        }
    }

    g_imageEffectSuite->clipReleaseImage(image);
    if (g_progressSuite) {
        g_progressSuite->progressEnd((OfxImageEffectHandle)inst);
    }
    return kOfxStatOK;
}

/* Effect entry for the slow variant: render takes the slow path, other
 * actions delegate to the base implementation. */
static OfxStatus mainEntry(const char *action, const void *handle,
                           OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs);

static OfxStatus mainEntrySlow(const char *action, const void *handle,
                               OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    if (strcmp(action, kOfxImageEffectActionRender) == 0) {
        return actionRenderSlow((OfxImageEffectHandle)handle, inArgs);
    }
    return mainEntry(action, handle, inArgs, outArgs);
}
/* ---------- ofxColour (M11 §4): GetOutputColourspace ---------- */

static OfxStatus actionGetOutputColourspace(OfxPropertySetHandle inArgs,
                                            OfxPropertySetHandle outArgs)
{
    /* Prefer the host's first preferred colourspace; otherwise cross-reference
     * the Source clip (ofxColour.h: a cross-reference is a valid write-back
     * the host must resolve). */
    int n = 0;
    g_propSuite->propGetDimension(inArgs, kOfxImageClipPropPreferredColourspaces, &n);
    if (n > 0) {
        char *pref = NULL;
        OfxStatus st = g_propSuite->propGetString(
            inArgs, kOfxImageClipPropPreferredColourspaces, 0, &pref);
        if (st == kOfxStatOK && pref) {
            propSetString(outArgs, kOfxImageClipPropColourspace, 0, pref);
            return kOfxStatOK;
        }
    }
    propSetString(outArgs, kOfxImageClipPropColourspace, 0, "OfxColourspace_Source");
    return kOfxStatOK;
}

/* ---------- isIdentity variant: pass through Source ---------- */

static OfxStatus actionIsIdentity(OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    /* Always pass through Source (time-invariant) — the host should
     * short-circuit render and copy the Source frame directly. */
    propSetString(outArgs, kOfxImageEffectPropIsIdentity, 0, "Source");
    return kOfxStatOK;
}

/* ---------- GL variant (M11 §4): attach/detach and GL render ---------- */

static OfxStatus actionGLAttached(OfxImageEffectHandle handle)
{
    if (g_messageSuite) {
        g_messageSuite->message(handle, kOfxMessageMessage, "gl-test", "gl-attached");
    }
    return kOfxStatOK;
}

static OfxStatus actionGLDetached(OfxImageEffectHandle handle)
{
    if (g_messageSuite) {
        g_messageSuite->message(handle, kOfxMessageMessage, "gl-test", "gl-detached");
    }
    return kOfxStatOK;
}

/* GL render: fetch Source and Output textures via the OpenGL suite and report
 * their indices (host-side tests assert on captured messages). Falls back to
 * CPU render when GL is not enabled. */
static void draw_gl_test_pattern(void)
{
    /* Clear the currently bound framebuffer (the FBO the host attached for the
     * output frame) to a known colour. The Linux EGL/GL bridge is now a real
     * implementation too, so this is no longer Apple-only. */
    glClearColor(0.1f, 0.2f, 0.3f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
}

static OfxStatus actionRenderGL(OfxImageEffectHandle inst, OfxPropertySetHandle inArgs)
{
    int gl_enabled = 0;
    g_propSuite->propGetInt(inArgs, kOfxImageEffectPropOpenGLEnabled, 0, &gl_enabled);
    if (!gl_enabled) {
        return actionRender(inst, inArgs);
    }
    if (!g_glSuite) {
        return kOfxStatErrMissingHostFeature;
    }

    double time = 0.0;
    propGetDouble(inArgs, kOfxPropTime, 0, &time);

    /* Input clip: clipLoadTexture (request RGBA). */
    OfxImageClipHandle clip = NULL;
    OfxPropertySetHandle clipProps = NULL;
    OfxStatus st = g_imageEffectSuite->clipGetHandle(inst, "Source", &clip, &clipProps);
    if (st != kOfxStatOK)
        return st;
    OfxPropertySetHandle tex = NULL;
    st = g_glSuite->clipLoadTexture(clip, time, GL_FORMAT_RGBA, NULL, &tex);
    if (st != kOfxStatOK)
        return st;
    int src_index = 0;
    g_propSuite->propGetInt(tex, kOfxImageEffectPropOpenGLTextureIndex, 0, &src_index);
    if (g_messageSuite) {
        g_messageSuite->message(inst, kOfxMessageMessage, "gl-test", "gl-source-index=%d", src_index);
    }
    st = g_glSuite->clipFreeTexture(tex);
    if (st != kOfxStatOK)
        return st;

    /* Output clip: clipLoadTexture(Output) (format ignored). */
    st = g_imageEffectSuite->clipGetHandle(inst, "Output", &clip, &clipProps);
    if (st != kOfxStatOK)
        return st;
    tex = NULL;
    st = g_glSuite->clipLoadTexture(clip, time, NULL, NULL, &tex);
    if (st != kOfxStatOK)
        return st;
    int out_index = 0;
    g_propSuite->propGetInt(tex, kOfxImageEffectPropOpenGLTextureIndex, 0, &out_index);
    if (g_messageSuite) {
        g_messageSuite->message(inst, kOfxMessageMessage, "gl-test", "gl-output-index=%d", out_index);
    }
    st = g_glSuite->clipFreeTexture(tex);
    if (st != kOfxStatOK)
        return st;

    /* Real drawing: clear the host-bound output FBO to a known colour
     * (0.1, 0.2, 0.3, 1.0) — the host's glReadPixels readback should return
     * that colour, end-to-end proof that the plugin really takes the GL path
     * and produces correct output. */
    draw_gl_test_pattern();

    return kOfxStatOK;
}

static OfxStatus mainEntry(const char *action, const void *handle, OfxPropertySetHandle inArgs,
                           OfxPropertySetHandle outArgs)
{
    if (strcmp(action, kOfxActionDescribe) == 0 ||
        strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
        return actionDescribe(handle, 0);
    }
    if (strcmp(action, kOfxActionCreateInstance) == 0) {
        /* Send one message via the message suite (for facade handler
         * assertions). */
        if (g_messageSuite) {
            g_messageSuite->message(handle, kOfxMessageMessage, "test-plugin",
                                    "created id=%d", 7);
        }
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionDestroyInstance) == 0 ||
        strcmp(action, kOfxActionLoad) == 0 ||
        strcmp(action, kOfxActionUnload) == 0 ||
        strcmp(action, kOfxImageEffectActionBeginSequenceRender) == 0 ||
        strcmp(action, kOfxImageEffectActionEndSequenceRender) == 0 ||
        strcmp(action, kOfxImageEffectActionIsIdentity) == 0) {
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxImageEffectActionGetClipPreferences) == 0) {
        return actionGetClipPreferences(outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetRegionOfDefinition) == 0) {
        return actionGetRoD(outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetRegionsOfInterest) == 0) {
        return actionGetRoI((OfxImageEffectHandle)handle, inArgs, outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetOutputColourspace) == 0) {
        return actionGetOutputColourspace(inArgs, outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionRender) == 0) {
        return actionRender((OfxImageEffectHandle)handle, inArgs);
    }
    if (strcmp(action, kOfxActionInstanceChanged) == 0) {
        /* Host pressed the push button: kOfxPropName == "button",
         * kOfxPropChangeReason == kOfxChangeUserEdited. Count the button only
         * (other parameter changes stay silent). */
        char *name = NULL;
        g_propSuite->propGetString(inArgs, kOfxPropName, 0, &name);
        if (name && strcmp(name, "button") == 0) {
            record_button_instance_changed();
        }
        return kOfxStatOK;
    }
    return kOfxStatReplyDefault;
}

/* GL variant entry: describe includes GL declarations; render goes through the
 * GL suite; attach/detach are reported. */
static OfxStatus mainEntryGL(const char *action, const void *handle,
                             OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    if (strcmp(action, kOfxActionDescribe) == 0 ||
        strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
        return actionDescribe(handle, 1);
    }
    if (strcmp(action, kOfxActionCreateInstance) == 0) {
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionDestroyInstance) == 0 ||
        strcmp(action, kOfxActionLoad) == 0 ||
        strcmp(action, kOfxActionUnload) == 0 ||
        strcmp(action, kOfxImageEffectActionBeginSequenceRender) == 0 ||
        strcmp(action, kOfxImageEffectActionEndSequenceRender) == 0 ||
        strcmp(action, kOfxImageEffectActionIsIdentity) == 0) {
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionOpenGLContextAttached) == 0) {
        return actionGLAttached((OfxImageEffectHandle)handle);
    }
    if (strcmp(action, kOfxActionOpenGLContextDetached) == 0) {
        return actionGLDetached((OfxImageEffectHandle)handle);
    }
    if (strcmp(action, kOfxImageEffectActionGetClipPreferences) == 0) {
        return actionGetClipPreferences(outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetRegionOfDefinition) == 0) {
        return actionGetRoD(outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetRegionsOfInterest) == 0) {
        return actionGetRoI((OfxImageEffectHandle)handle, inArgs, outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetOutputColourspace) == 0) {
        return actionGetOutputColourspace(inArgs, outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionRender) == 0) {
        return actionRenderGL((OfxImageEffectHandle)handle, inArgs);
    }
    return kOfxStatReplyDefault;
}

/* Always-identity variant entry: isIdentity returns Source; the rest matches
 * the CPU plugin. */
static OfxStatus mainEntryID(const char *action, const void *handle,
                             OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    if (strcmp(action, kOfxActionDescribe) == 0 ||
        strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
        return actionDescribe(handle, 0);
    }
    if (strcmp(action, kOfxImageEffectActionIsIdentity) == 0) {
        return actionIsIdentity(inArgs, outArgs);
    }
    if (strcmp(action, kOfxImageEffectActionGetOutputColourspace) == 0) {
        return actionGetOutputColourspace(inArgs, outArgs);
    }
    return mainEntry(action, handle, inArgs, outArgs);
}

/* ---------- interact variant (M11 §5): custom UI host validation ---------- */

/* interact call log: append to the file named by
 * QUERCUS_TEST_PLUGIN_INTERACT_MARKER (for host test assertions; silent when
 * the env var is unset). */
static void interact_record(const char *fmt, ...)
{
    const char *marker = marker_env("QUERCUS_TEST_PLUGIN_INTERACT_MARKER");
    if (!marker)
        return;
    FILE *f = fopen(marker, "a");
    if (!f)
        return;
    va_list ap;
    va_start(ap, fmt);
    vfprintf(f, fmt, ap);
    va_end(ap);
    fclose(f);
}

/* Record the real pen event arguments (inArgs canonical/viewport/pressure
 * properties). */
static void record_pen(const char *action, OfxPropertySetHandle inArgs)
{
    int vp[2] = { 0, 0 };
    double canon[2] = { 0.0, 0.0 };
    double pressure = 0.0;
    g_propSuite->propGetIntN(inArgs, kOfxInteractPropPenViewportPosition, 2, vp);
    g_propSuite->propGetDoubleN(inArgs, kOfxInteractPropPenPosition, 2, canon);
    g_propSuite->propGetDouble(inArgs, kOfxInteractPropPenPressure, 0, &pressure);
    interact_record("%s vp=%d,%d canon=%g,%g pressure=%g\n", action, vp[0], vp[1], canon[0],
                    canon[1], pressure);
}

/* Record the real key event arguments (keySym/keyString). */
static void record_key(const char *action, OfxPropertySetHandle inArgs)
{
    int sym = 0;
    char *s = NULL;
    g_propSuite->propGetInt(inArgs, kOfxPropKeySym, 0, &sym);
    g_propSuite->propGetString(inArgs, kOfxPropKeyString, 0, &s);
    interact_record("%s sym=%d str=%s\n", action, sym, s ? s : "");
}

/* kOfxInteractActionDraw: read inArgs (viewport/pixelScale/time/
 * backgroundImage/drawContext), setColour via the Draw suite + draw a known
 * colour swatch with native GL (rectangle 10..30 canonical). Real drawing on
 * macOS; non-macOS skips GL. Returns OK. */
static OfxStatus actionInteractDraw(OfxPropertySetHandle inArgs)
{
    double vp[2] = { 0.0, 0.0 };
    double scale[2] = { 1.0, 1.0 };
    double t = 0.0;
    void *bg = NULL;
    void *drawCtx = NULL;
    g_propSuite->propGetDoubleN(inArgs, kOfxInteractPropViewportSize, 2, vp);
    g_propSuite->propGetDoubleN(inArgs, kOfxInteractPropPixelScale, 2, scale);
    g_propSuite->propGetDouble(inArgs, kOfxPropTime, 0, &t);
    g_propSuite->propGetPointer(inArgs, kOfxInteractPropBackgroundImage, 0, &bg);
    g_propSuite->propGetPointer(inArgs, kOfxInteractPropDrawContext, 0, &drawCtx);

    /* Real read/write of Draw suite state: setColour writes, getColour reads
     * (host colour palette). */
    double setcol[4] = { 0.0, 0.0, 0.0, 0.0 };
    double getcol[4] = { 0.0, 0.0, 0.0, 0.0 };
    int setcol_st = kOfxStatFailed, getcol_st = kOfxStatFailed;
    if (g_drawSuite && drawCtx) {
        OfxRGBAColourF col = { 0.9f, 0.1f, 0.2f, 1.0f };
        setcol_st = g_drawSuite->setColour(drawCtx, &col);
        setcol[0] = col.r;
        setcol[1] = col.g;
        setcol[2] = col.b;
        setcol[3] = col.a;
        OfxRGBAColourF bgc = { 0.0f, 0.0f, 0.0f, 0.0f };
        getcol_st = g_drawSuite->getColour(drawCtx, kOfxStandardColourOverlayBackground, &bgc);
        getcol[0] = bgc.r;
        getcol[1] = bgc.g;
        getcol[2] = bgc.b;
        getcol[3] = bgc.a;
    }

    int draw_st = kOfxStatFailed;
#ifdef __APPLE__
    /* Clear to a dark background + Draw suite draws a filled rectangle
     * (canonical coordinates; the host maps them to the viewport via an
     * orthographic projection). */
    glClearColor(0.05f, 0.05f, 0.05f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    if (g_drawSuite && drawCtx) {
        OfxPointD pts[2] = { { 10.0, 10.0 }, { 30.0, 30.0 } };
        draw_st = g_drawSuite->draw(drawCtx, kOfxDrawPrimitiveRectangle, pts, 2);
    }
#else
    (void)0;
#endif

    interact_record("draw vp=%gx%g scale=%gx%g t=%g bg=%p setcol_st=%d setcol=%g,%g,%g,%g "
                    "getcol_st=%d getcol=%g,%g,%g,%g draw_st=%d\n",
                    vp[0], vp[1], scale[0], scale[1], t, bg, setcol_st, setcol[0], setcol[1],
                    setcol[2], setcol[3], getcol_st, getcol[0], getcol[1], getcol[2], getcol[3],
                    draw_st);
    return kOfxStatOK;
}

/* interact entry (declared via kOfxImageEffectPluginPropOverlayInteractV2):
 * handles interact actions only; effect-side actions always ReplyDefault
 * (the host never calls here with an effect handle). */
static OfxStatus mainEntryInteract(const char *action, const void *handle,
                                   OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    (void)handle;
    (void)outArgs;
    if (strcmp(action, kOfxActionNewInteract) == 0) {
        interact_record("new_interact\n");
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionDescribe) == 0) {
        interact_record("describe\n");
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionCreateInstance) == 0) {
        interact_record("create\n");
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxActionDestroyInstance) == 0) {
        interact_record("destroy\n");
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionDraw) == 0) {
        return actionInteractDraw(inArgs);
    }
    if (strcmp(action, kOfxInteractActionPenMotion) == 0) {
        record_pen("pen_motion", inArgs);
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionPenDown) == 0) {
        record_pen("pen_down", inArgs);
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionPenUp) == 0) {
        record_pen("pen_up", inArgs);
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionKeyDown) == 0) {
        int sym = 0;
        g_propSuite->propGetInt(inArgs, kOfxPropKeySym, 0, &sym);
        record_key("key_down", inArgs);
        /* Escape → ReplyDefault (the host should pass that status through:
         * unhandled by the plugin, the event can go to other objects in the
         * view). */
        if (sym == kOfxKey_Escape)
            return kOfxStatReplyDefault;
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionKeyUp) == 0) {
        record_key("key_up", inArgs);
        return kOfxStatOK;
    }
    if (strcmp(action, kOfxInteractActionIdle) == 0) {
        interact_record("idle\n");
        return kOfxStatOK;
    }
    /* GainFocus/LoseFocus etc.: default (ReplyDefault = unhandled, the host may
     * continue). */
    return kOfxStatReplyDefault;
}

/* Effect-side main entry of the interact variant: describe creates the
 * parameters/clips and declares the overlay interact V2 entry (pointing at
 * mainEntryInteract); other effect actions delegate to base mainEntry. */
static OfxStatus mainEntryInteractEffect(const char *action, const void *handle,
                                         OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    if (strcmp(action, kOfxActionDescribe) == 0 ||
        strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
        OfxStatus st = actionDescribe(handle, 0);
        if (st != kOfxStatOK)
            return st;
        return propSetPointer(handle, kOfxImageEffectPluginPropOverlayInteractV2, 0,
                              (void *)mainEntryInteract);
    }
    return mainEntry(action, handle, inArgs, outArgs);
}

/* ---------- mask variant: optional mask clip fixture ----------
 *
 * Filter context, clips: Source, Mask (OfxImageClipPropOptional=1 and
 * OfxImageClipPropIsMask=1), Output. Documented deterministic behaviour:
 *  - Mask connected (OfxImageClipPropConnected=1): output RGB = Source RGB
 *    x Mask red channel, alpha = Source alpha;
 *  - Mask not connected: pass Source through unchanged (output == Source).
 * Every other action matches the CPU plugin. */
static OfxStatus actionDescribeMask(const void *handle)
{
    OfxStatus st = actionDescribe(handle, 0);
    if (st != kOfxStatOK)
        return st;

    const char *comps[] = { kOfxImageComponentRGBA, kOfxImageComponentRGB, kOfxImageComponentAlpha };
    OfxPropertySetHandle clipProps = NULL;
    st = g_imageEffectSuite->clipDefine((OfxImageEffectHandle)handle, "Mask", &clipProps);
    if (st != kOfxStatOK)
        return st;
    propSetStringN(clipProps, kOfxImageEffectPropSupportedComponents, 3, comps);
    propSetString(clipProps, kOfxPropLabel, 0, "Mask");
    g_propSuite->propSetInt(clipProps, kOfxImageClipPropOptional, 0, 1);
    g_propSuite->propSetInt(clipProps, kOfxImageClipPropIsMask, 0, 1);
    return kOfxStatOK;
}

/* Fetch the clip image base/row stride/bounds; non-OK on failure. */
static OfxStatus getClipPixels(OfxImageClipHandle clip, double time, OfxPropertySetHandle *image,
                               float **data, int *rowBytes, int bounds[4])
{
    void *ptr = NULL;
    OfxStatus st = g_imageEffectSuite->clipGetImage(clip, time, NULL, image);
    if (st != kOfxStatOK)
        return st;
    propGetPointer(*image, kOfxImagePropData, 0, &ptr);
    propGetInt(*image, kOfxImagePropRowBytes, 0, rowBytes);
    propGetIntN(*image, kOfxImagePropBounds, 4, bounds);
    *data = (float *)ptr;
    return kOfxStatOK;
}

static OfxStatus actionRenderMask(OfxImageEffectHandle inst, OfxPropertySetHandle inArgs)
{
    double time = 0.0;
    propGetDouble(inArgs, kOfxPropTime, 0, &time);

    OfxImageClipHandle outClip = NULL, srcClip = NULL, maskClip = NULL;
    OfxPropertySetHandle outProps = NULL, srcProps = NULL, maskProps = NULL;
    OfxStatus st = g_imageEffectSuite->clipGetHandle(inst, "Output", &outClip, &outProps);
    if (st != kOfxStatOK)
        return st;
    st = g_imageEffectSuite->clipGetHandle(inst, "Source", &srcClip, &srcProps);
    if (st != kOfxStatOK)
        return st;
    st = g_imageEffectSuite->clipGetHandle(inst, "Mask", &maskClip, &maskProps);
    if (st != kOfxStatOK)
        return st;

    int maskConnected = 0;
    propGetInt(maskProps, kOfxImageClipPropConnected, 0, &maskConnected);

    OfxPropertySetHandle outImg = NULL, srcImg = NULL, maskImg = NULL;
    float *out = NULL, *src = NULL, *mask = NULL;
    int outRow = 0, srcRow = 0, maskRow = 0;
    int outB[4] = { 0, 0, 0, 0 }, srcB[4] = { 0, 0, 0, 0 }, maskB[4] = { 0, 0, 0, 0 };

    st = getClipPixels(outClip, time, &outImg, &out, &outRow, outB);
    if (st != kOfxStatOK)
        return st;
    st = getClipPixels(srcClip, time, &srcImg, &src, &srcRow, srcB);
    if (st == kOfxStatOK && maskConnected) {
        st = getClipPixels(maskClip, time, &maskImg, &mask, &maskRow, maskB);
    }
    if (st == kOfxStatOK && out && src) {
        int w = outB[2] - outB[0];
        int h = outB[3] - outB[1];
        int sw = srcB[2] - srcB[0];
        int sh = srcB[3] - srcB[1];
        if (sw < w) w = sw;
        if (sh < h) h = sh;
        if (mask) {
            int mw = maskB[2] - maskB[0];
            int mh = maskB[3] - maskB[1];
            if (mw < w) w = mw;
            if (mh < h) h = mh;
        }
        for (int y = 0; y < h; y++) {
            float *orow = (float *)((char *)out + (size_t)y * (size_t)outRow);
            const float *srow = (const float *)((const char *)src + (size_t)y * (size_t)srcRow);
            const float *mrow =
                mask ? (const float *)((const char *)mask + (size_t)y * (size_t)maskRow) : NULL;
            for (int x = 0; x < w; x++) {
                float k = mrow ? mrow[x * 4 + 0] : 1.0f;
                orow[x * 4 + 0] = srow[x * 4 + 0] * k;
                orow[x * 4 + 1] = srow[x * 4 + 1] * k;
                orow[x * 4 + 2] = srow[x * 4 + 2] * k;
                orow[x * 4 + 3] = srow[x * 4 + 3];
            }
        }
    }

    if (maskImg)
        g_imageEffectSuite->clipReleaseImage(maskImg);
    if (srcImg)
        g_imageEffectSuite->clipReleaseImage(srcImg);
    g_imageEffectSuite->clipReleaseImage(outImg);
    return st;
}

static OfxStatus mainEntryMask(const char *action, const void *handle,
                               OfxPropertySetHandle inArgs, OfxPropertySetHandle outArgs)
{
    if (strcmp(action, kOfxActionDescribe) == 0 ||
        strcmp(action, kOfxImageEffectActionDescribeInContext) == 0) {
        return actionDescribeMask(handle);
    }
    if (strcmp(action, kOfxImageEffectActionRender) == 0) {
        return actionRenderMask((OfxImageEffectHandle)handle, inArgs);
    }
    return mainEntry(action, handle, inArgs, outArgs);
}

/* ---------- exports ---------- */

static const OfxPlugin test_plugin = {
    /* pluginApi */ kOfxImageEffectPluginApi,
    /* apiVersion */ kOfxImageEffectPluginApiVersion,
    /* pluginIdentifier */ "org.quercus.test-plugin",
    /* pluginVersionMajor */ 1,
    /* pluginVersionMinor */ 0,
    /* setHost */ setHost,
    /* mainEntry */ mainEntry,
};

static const OfxPlugin test_plugin_gl = {
    /* pluginApi */ kOfxImageEffectPluginApi,
    /* apiVersion */ kOfxImageEffectPluginApiVersion,
    /* pluginIdentifier */ "org.quercus.test-plugin.gl",
    /* pluginVersionMajor */ 1,
    /* pluginVersionMinor */ 0,
    /* setHost */ setHost,
    /* mainEntry */ mainEntryGL,
};

static const OfxPlugin test_plugin_id = {
    /* pluginApi */ kOfxImageEffectPluginApi,
    /* apiVersion */ kOfxImageEffectPluginApiVersion,
    /* pluginIdentifier */ "org.quercus.test-plugin.identity",
    /* pluginVersionMajor */ 1,
    /* pluginVersionMinor */ 0,
    /* setHost */ setHost,
    /* mainEntry */ mainEntryID,
};

static const OfxPlugin test_plugin_interact = {
    /* pluginApi */ kOfxImageEffectPluginApi,
    /* apiVersion */ kOfxImageEffectPluginApiVersion,
    /* pluginIdentifier */ "org.quercus.test-plugin.interact",
    /* pluginVersionMajor */ 1,
    /* pluginVersionMinor */ 0,
    /* setHost */ setHost,
    /* mainEntry */ mainEntryInteractEffect,
};

/* M3 cancellation e2e: slow filter (progressUpdate x20ms loop, aborts on
 * cancellation). */
static const OfxPlugin test_plugin_slow = {
    /* pluginApi */ kOfxImageEffectPluginApi,
    /* apiVersion */ kOfxImageEffectPluginApiVersion,
    /* pluginIdentifier */ "org.quercus.test-plugin.slow",
    /* pluginVersionMajor */ 1,
    /* pluginVersionMinor */ 0,
    /* setHost */ setHost,
    /* mainEntry */ mainEntrySlow,
};

/* Optional-mask clip fixture (Resolve 21 gap: optional OFX mask input). */
static const OfxPlugin test_plugin_mask = {
    /* pluginApi */ kOfxImageEffectPluginApi,
    /* apiVersion */ kOfxImageEffectPluginApiVersion,
    /* pluginIdentifier */ "org.quercus.test-plugin.mask",
    /* pluginVersionMajor */ 1,
    /* pluginVersionMinor */ 0,
    /* setHost */ setHost,
    /* mainEntry */ mainEntryMask,
};

OfxExport int OfxGetNumberOfPlugins(void)
{
    return 6;
}

OfxExport OfxPlugin *OfxGetPlugin(int nth)
{
    if (nth == 0)
        return (OfxPlugin *)&test_plugin;
    if (nth == 1)
        return (OfxPlugin *)&test_plugin_gl;
    if (nth == 2)
        return (OfxPlugin *)&test_plugin_id;
    if (nth == 3)
        return (OfxPlugin *)&test_plugin_interact;
    if (nth == 4)
        return (OfxPlugin *)&test_plugin_slow;
    if (nth == 5)
        return (OfxPlugin *)&test_plugin_mask;
    return NULL;
}
