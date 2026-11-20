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
 * C entry points for the OFX Message suite. Stable Rust cannot define
 * C-variadic functions (c_variadic is still unstable): v1's '...' and v2's
 * va_list are both vsnprintf'd into a fixed-size buffer here, then forwarded
 * to the Rust implementation quercus_ofx_message_impl.
 *
 * Build: build.rs (cc crate); the symbols reach libquercusplugin via the
 * staticlib (corrosion needs no extra link wiring at link time).
 */

#include <stdarg.h>
#include <stdio.h>

extern int quercus_ofx_message_impl(void *handle, const char *type,
                                const char *id, const char *message);

/* Message buffer limit (mirrors the C++ side's 1024 convention in
 * format_message, olivehost.cpp:99; overlong output is truncated without
 * error). */
#define QUERCUS_MSG_BUF_SIZE 1024

static int forward(void *handle, const char *type, const char *id,
                   const char *format, va_list args)
{
    if (!format) {
        /* C++ side: !format → kOfxStatFailed (olivehost.cpp:267); signal the
         * Rust side with a NULL message. */
        return quercus_ofx_message_impl(handle, type, id, NULL);
    }

    char buf[QUERCUS_MSG_BUF_SIZE];
    int n = vsnprintf(buf, sizeof(buf), format, args);
    if (n < 0) {
        return quercus_ofx_message_impl(handle, type, id, NULL);
    }
    return quercus_ofx_message_impl(handle, type, id, buf);
}

int ofx_message_shim_v1(void *handle, const char *type, const char *id,
                        const char *format, ...)
{
    va_list args;
    va_start(args, format);
    int r = forward(handle, type, id, format, args);
    va_end(args);
    return r;
}

int ofx_message_shim_v2(void *handle, const char *type, const char *id,
                        const char *format, va_list args)
{
    return forward(handle, type, id, format, args);
}
