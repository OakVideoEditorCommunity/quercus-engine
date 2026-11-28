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
 * Variadic entry points for OfxParameterSuite. Stable Rust cannot define
 * C-variadic functions (c_variadic is still unstable): the '...' of
 * paramGetValue / paramSetValue / *AtTime / derivative / integral is
 * decoded here by parameter type (type queried via the Rust-exported
 * ofx_param_kind_of), then forwarded to the typed Rust implementation.
 *
 * Variadic argument shapes (HS: getV/setV of each type in ofxhParam.cpp):
 *   get: Integer/Boolean/Choice → int*; Double → double*;
 *        2D/3D/colour → sequence of int* / double* pointers, one per dim;
 *        String → char** (writes the resident pointer)
 *   set: same shapes but by value (sequence of int/double values;
 *        String → char*)
 *
 * KIND_* matches the Rust-side param::ParamKind enum one-to-one.
 */

#include <stdarg.h>

/* Rust exports */
extern int ofx_param_kind_of(void *param);
extern int ofx_param_invalid_kind_impl(void *param);
extern int ofx_param_get_impl(void *param, int kind, void *out);
extern int ofx_param_get_at_time_impl(void *param, double time, int kind, void *out);
extern int ofx_param_set_impl(void *param, int kind, const void *in);
extern int ofx_param_set_at_time_impl(void *param, double time, int kind, const void *in);
extern int ofx_param_get_string_impl(void *param, char **out);
extern int ofx_param_get_string_at_time_impl(void *param, double time, char **out);
extern int ofx_param_set_string_impl(void *param, const char *in);
extern int ofx_param_set_string_at_time_impl(void *param, double time, const char *in);
extern int ofx_param_missing_feature_impl(void *param);

enum {
    KIND_INT = 1,
    KIND_INT2 = 2,
    KIND_INT3 = 3,
    KIND_DOUBLE = 4,
    KIND_DOUBLE2 = 5,
    KIND_DOUBLE3 = 6,
    KIND_BOOL = 7,
    KIND_CHOICE = 8,
    KIND_RGB = 9,
    KIND_RGBA = 10,
    KIND_STRING = 11,
    KIND_STRCHOICE = 12
};

static int call_get_numeric(void *param, int has_time, double time, int kind, void *out)
{
    return has_time
        ? ofx_param_get_at_time_impl(param, time, kind, out)
        : ofx_param_get_impl(param, kind, out);
}

static int call_get_string(void *param, int has_time, double time, char **out)
{
    return has_time
        ? ofx_param_get_string_at_time_impl(param, time, out)
        : ofx_param_get_string_impl(param, out);
}

static int call_set_numeric(void *param, int has_time, double time, int kind, const void *in)
{
    return has_time
        ? ofx_param_set_at_time_impl(param, time, kind, in)
        : ofx_param_set_impl(param, kind, in);
}

static int call_set_string(void *param, int has_time, double time, const char *in)
{
    return has_time
        ? ofx_param_set_string_at_time_impl(param, time, in)
        : ofx_param_set_string_impl(param, in);
}

/* get: gather the pointer sequence from the plugin into a local buffer, call
 * Rust, then scatter back on success. */
static int get_dispatch(void *param, int has_time, double time, int kind, va_list ap)
{
    if (kind == 0) {
        return ofx_param_invalid_kind_impl(param);
    }
    switch (kind) {
    case KIND_INT:
    case KIND_BOOL:
    case KIND_CHOICE: {
        int *v = va_arg(ap, int *);
        return call_get_numeric(param, has_time, time, kind, v);
    }
    case KIND_INT2: {
        int *a = va_arg(ap, int *);
        int *b = va_arg(ap, int *);
        if (!a || !b) {
            return 11; /* kOfxStatErrValue */
        }
        int buf[2] = { 0, 0 };
        int r = call_get_numeric(param, has_time, time, kind, buf);
        if (r == 0) {
            *a = buf[0];
            *b = buf[1];
        }
        return r;
    }
    case KIND_INT3: {
        int *a = va_arg(ap, int *);
        int *b = va_arg(ap, int *);
        int *c = va_arg(ap, int *);
        if (!a || !b || !c) {
            return 11; /* kOfxStatErrValue */
        }
        int buf[3] = { 0, 0, 0 };
        int r = call_get_numeric(param, has_time, time, kind, buf);
        if (r == 0) {
            *a = buf[0];
            *b = buf[1];
            *c = buf[2];
        }
        return r;
    }
    case KIND_DOUBLE: {
        double *v = va_arg(ap, double *);
        return call_get_numeric(param, has_time, time, kind, v);
    }
    case KIND_DOUBLE2: {
        double *a = va_arg(ap, double *);
        double *b = va_arg(ap, double *);
        if (!a || !b) {
            return 11; /* kOfxStatErrValue */
        }
        double buf[2] = { 0.0, 0.0 };
        int r = call_get_numeric(param, has_time, time, kind, buf);
        if (r == 0) {
            *a = buf[0];
            *b = buf[1];
        }
        return r;
    }
    case KIND_DOUBLE3: {
        double *a = va_arg(ap, double *);
        double *b = va_arg(ap, double *);
        double *c = va_arg(ap, double *);
        if (!a || !b || !c) {
            return 11; /* kOfxStatErrValue */
        }
        double buf[3] = { 0.0, 0.0, 0.0 };
        int r = call_get_numeric(param, has_time, time, kind, buf);
        if (r == 0) {
            *a = buf[0];
            *b = buf[1];
            *c = buf[2];
        }
        return r;
    }
    case KIND_RGB: {
        double *a = va_arg(ap, double *);
        double *b = va_arg(ap, double *);
        double *c = va_arg(ap, double *);
        if (!a || !b || !c) {
            return 11; /* kOfxStatErrValue */
        }
        double buf[3] = { 0.0, 0.0, 0.0 };
        int r = call_get_numeric(param, has_time, time, kind, buf);
        if (r == 0) {
            *a = buf[0];
            *b = buf[1];
            *c = buf[2];
        }
        return r;
    }
    case KIND_RGBA: {
        double *a = va_arg(ap, double *);
        double *b = va_arg(ap, double *);
        double *c = va_arg(ap, double *);
        double *d = va_arg(ap, double *);
        if (!a || !b || !c || !d) {
            return 11; /* kOfxStatErrValue */
        }
        double buf[4] = { 0.0, 0.0, 0.0, 0.0 };
        int r = call_get_numeric(param, has_time, time, kind, buf);
        if (r == 0) {
            *a = buf[0];
            *b = buf[1];
            *c = buf[2];
            *d = buf[3];
        }
        return r;
    }
    case KIND_STRING:
    case KIND_STRCHOICE: {
        char **p = va_arg(ap, char **);
        return call_get_string(param, has_time, time, p);
    }
    default:
        return 1; /* kOfxStatFailed: unknown kind */
    }
}

/* set: gather by value into a local buffer, call Rust. */
static int set_dispatch(void *param, int has_time, double time, int kind, va_list ap)
{
    if (kind == 0) {
        return ofx_param_invalid_kind_impl(param);
    }
    switch (kind) {
    case KIND_INT:
    case KIND_BOOL:
    case KIND_CHOICE: {
        int v = va_arg(ap, int);
        return call_set_numeric(param, has_time, time, kind, &v);
    }
    case KIND_INT2: {
        int a = va_arg(ap, int);
        int b = va_arg(ap, int);
        int buf[2] = { a, b };
        return call_set_numeric(param, has_time, time, kind, buf);
    }
    case KIND_INT3: {
        int a = va_arg(ap, int);
        int b = va_arg(ap, int);
        int c = va_arg(ap, int);
        int buf[3] = { a, b, c };
        return call_set_numeric(param, has_time, time, kind, buf);
    }
    case KIND_DOUBLE: {
        double v = va_arg(ap, double);
        return call_set_numeric(param, has_time, time, kind, &v);
    }
    case KIND_DOUBLE2: {
        double a = va_arg(ap, double);
        double b = va_arg(ap, double);
        double buf[2] = { a, b };
        return call_set_numeric(param, has_time, time, kind, buf);
    }
    case KIND_DOUBLE3: {
        double a = va_arg(ap, double);
        double b = va_arg(ap, double);
        double c = va_arg(ap, double);
        double buf[3] = { a, b, c };
        return call_set_numeric(param, has_time, time, kind, buf);
    }
    case KIND_RGB: {
        double a = va_arg(ap, double);
        double b = va_arg(ap, double);
        double c = va_arg(ap, double);
        double buf[3] = { a, b, c };
        return call_set_numeric(param, has_time, time, kind, buf);
    }
    case KIND_RGBA: {
        double a = va_arg(ap, double);
        double b = va_arg(ap, double);
        double c = va_arg(ap, double);
        double d = va_arg(ap, double);
        double buf[4] = { a, b, c, d };
        return call_set_numeric(param, has_time, time, kind, buf);
    }
    case KIND_STRING:
    case KIND_STRCHOICE: {
        const char *s = va_arg(ap, const char *);
        return call_set_string(param, has_time, time, s);
    }
    default:
        return 1; /* kOfxStatFailed */
    }
}

int ofx_param_get_value_shim(void *param, ...)
{
    va_list ap;
    va_start(ap, param);
    int kind = ofx_param_kind_of(param);
    int r = get_dispatch(param, 0, 0.0, kind, ap);
    va_end(ap);
    return r;
}

int ofx_param_get_value_at_time_shim(void *param, double time, ...)
{
    va_list ap;
    va_start(ap, time);
    int kind = ofx_param_kind_of(param);
    int r = get_dispatch(param, 1, time, kind, ap);
    va_end(ap);
    return r;
}

int ofx_param_set_value_shim(void *param, ...)
{
    va_list ap;
    va_start(ap, param);
    int kind = ofx_param_kind_of(param);
    int r = set_dispatch(param, 0, 0.0, kind, ap);
    va_end(ap);
    return r;
}

int ofx_param_set_value_at_time_shim(void *param, double time, ...)
{
    va_list ap;
    va_start(ap, time);
    int kind = ofx_param_kind_of(param);
    int r = set_dispatch(param, 1, time, kind, ap);
    va_end(ap);
    return r;
}

/* derivative/integral: deferred to follow-up work when interpolation derivatives/integrals
 * are implemented; consumes no va_args and returns kOfxStatErrMissingHostFeature. */
int ofx_param_get_derivative_shim(void *param, double time, ...)
{
    (void)time;
    return ofx_param_missing_feature_impl(param);
}

int ofx_param_get_integral_shim(void *param, double time1, double time2, ...)
{
    (void)time1;
    (void)time2;
    return ofx_param_missing_feature_impl(param);
}
