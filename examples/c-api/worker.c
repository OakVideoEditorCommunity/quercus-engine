/* SPDX-License-Identifier: GPL-3.0-or-later */
/*
 * Thin quercus-worker launcher built from the public C ABI.
 *
 * Export and preview rendering run in `quercus-worker` child processes. The
 * engine looks for that binary through (in order) its dispatcher config,
 * QUERCUS_WORKER_BIN, or a `quercus-worker` file next to the current
 * executable. If your install does not ship the worker binary, this launcher
 * provides one: it forwards `--backend <name>` (the only argument the engine
 * passes; `auto` when omitted) to quercus_worker_run, which runs the worker
 * control loop on stdin/stdout.
 *
 * Build it next to the example binaries and point QUERCUS_WORKER_BIN at it,
 * or name it `quercus-worker` beside the binary that starts an export.
 */
#include <stdint.h>
#include <string.h>

#include "quercus_engine.h"

int main(int argc, char **argv)
{
    const char *backend = "auto";
    int i;

    for (i = 1; i + 1 < argc; i++) {
        if (strcmp(argv[i], "--backend") == 0) {
            backend = argv[i + 1];
            i++;
        }
    }

    return quercus_worker_run((const uint8_t *)backend, strlen(backend));
}
