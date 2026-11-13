/* SPDX-License-Identifier: GPL-3.0-or-later */
/*
 * Quercus engine C ABI quickstart.
 *
 * Creates an editor session, creates a sequence, optionally imports a media
 * file, sets the timeline work area, and saves the project as a .ove file.
 * Only the generated public header (quercus_engine.h) is used.
 *
 * Usage:
 *   quickstart [media-file] [output.ove]
 *
 *   media-file   optional path passed to quercus_footage_import
 *   output.ove   optional save path (default: quercus-quickstart.ove)
 *
 * The C ABI has no global "last error" function. Fallible calls report
 * failure through their return value and, where the header says so, an error
 * buffer (quercus_editor_edit) or a per-task error query
 * (quercus_task_error). This example does not invent an engine-wide error
 * string.
 */
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "quercus_engine.h"

#define BUF_CAP 1024

/* All string getters in the ABI are snprintf-style: they write a
   NUL-terminated value into the buffer, return the full byte length excluding
   the NUL, and return -1 on a null/missing handle. A return value greater
   than or equal to the buffer size means the output was truncated. */
static void print_string_value(const char *label, int32_t len,
                               const uint8_t *buf, size_t cap)
{
    if (len < 0) {
        printf("  %-18s <error %d>\n", label, len);
        return;
    }
    printf("  %-18s %s%s\n", label, (const char *)buf,
           (size_t)len >= cap ? " (truncated)" : "");
}

static void print_rational(const char *label, Rational r)
{
    if (r.den == 0 || r.den == 1) {
        printf("  %-18s %f s\n", label, quercus_core_rational_to_f64(r));
    } else {
        printf("  %-18s %f s (%lld/%lld)\n", label,
               quercus_core_rational_to_f64(r), (long long)r.num,
               (long long)r.den);
    }
}

static int fail(const char *step)
{
    fprintf(stderr, "FAILED: %s\n", step);
    return 1;
}

int main(int argc, char **argv)
{
    const char *media_path = argc > 1 ? argv[1] : NULL;
    const char *save_path = argc > 2 ? argv[2] : "quercus-quickstart.ove";
    const uint64_t invalid = quercus_node_id_invalid();
    void *session;
    void *project;
    uint64_t sequence;
    QuercusEditResult edit_result;

    printf("Quercus engine C ABI quickstart\n");

    /* 1. Editor session: owns a project, its current sequence, the undo
       stack, markers and the work area. */
    session = quercus_editor_new();
    if (session == NULL) {
        return fail("quercus_editor_new");
    }
    printf("  editor session created\n");

    /* 2. Borrow the session's live project (refcounted: release it below). */
    project = quercus_editor_project(session);
    if (project == NULL) {
        quercus_editor_release(session);
        return fail("quercus_editor_project");
    }
    {
        uint8_t name[BUF_CAP];
        int32_t len = quercus_project_name(project, name, sizeof(name));
        print_string_value("project name", len, name, sizeof(name));
    }

    /* 3. Create a sequence. The engine gives it the default V1/V2/A1/A2
       track layout and makes it the session's current sequence. */
    sequence = quercus_editor_create_sequence(
        session, (const uint8_t *)"Quickstart", strlen("Quickstart"));
    if (sequence == invalid) {
        quercus_project_release(project);
        quercus_editor_release(session);
        return fail("quercus_editor_create_sequence");
    }
    printf("  sequence id       %llu (current: %llu)\n",
           (unsigned long long)sequence,
           (unsigned long long)quercus_editor_current_sequence(session));

    {
        int32_t width = 0;
        int32_t height = 0;
        Rational fps = quercus_sequence_frame_rate(project, sequence);
        Rational length = quercus_sequence_length(project, sequence);

        if (quercus_sequence_geometry(project, sequence, &width, &height)) {
            printf("  sequence geometry %dx%d\n", width, height);
        }
        printf("  sequence fps      %lld/%lld\n", (long long)fps.num,
               (long long)fps.den);
        print_rational("sequence length", length);
    }

    /* 4. Import a footage file when one was supplied. The call probes the
       file synchronously and returns an invalid node id on failure; there
       is no error string for this entry point. */
    if (media_path == NULL) {
        printf("  footage import    skipped (no media file argument)\n");
    } else {
        uint64_t footage = quercus_footage_import(
            project, (const uint8_t *)media_path, strlen(media_path));

        if (footage == invalid) {
            fprintf(stderr,
                    "FAILED: quercus_footage_import(\"%s\") returned an "
                    "invalid node id\n",
                    media_path);
            quercus_project_release(project);
            quercus_editor_release(session);
            return 1;
        }
        printf("  footage id        %llu\n", (unsigned long long)footage);
        {
            uint8_t name[BUF_CAP];
            int32_t len = quercus_footage_filename(project, footage, name,
                                                   sizeof(name));
            print_string_value("footage filename", len, name, sizeof(name));
        }
        print_rational("footage duration",
                       quercus_footage_duration(project, footage));
        printf("  video streams     %d\n",
               quercus_footage_video_stream_count(project, footage));
        printf("  audio streams     %d\n",
               quercus_footage_audio_stream_count(project, footage));
    }

    /* 5. Set the work area [0, 2) seconds. There is no dedicated
       quercus_editor_workarea_set function in this header: the work area is
       written through the generic edit dispatcher with
       QUERCUS_EDITOP_WORKAREASET, then read back with
       quercus_editor_workarea. */
    memset(&edit_result, 0, sizeof(edit_result));
    {
        QuercusEditParams params;
        uint8_t error[512];
        TimeRange range;
        TimeRange readback;
        int32_t code;

        memset(&params, 0, sizeof(params));
        memset(error, 0, sizeof(error));
        range.in_ = quercus_core_rational_new(0, 1);
        range.out = quercus_core_rational_new(2, 1);
        params.enabled = true;
        params.range = range;

        code = quercus_editor_edit(session, QUERCUS_EDITOP_WORKAREASET,
                                   &params, &edit_result, error,
                                   sizeof(error));
        if (code != 0) {
            fprintf(stderr, "FAILED: work area set returned %d: %s\n", code,
                    (const char *)error);
            quercus_project_release(project);
            quercus_editor_release(session);
            return 1;
        }

        if (quercus_editor_workarea(session, &readback)) {
            printf("  work area         [%f, %f) s\n",
                   quercus_core_rational_to_f64(readback.in_),
                   quercus_core_rational_to_f64(readback.out));
        } else {
            printf("  work area         disabled after set (unexpected)\n");
        }
    }

    /* 6. Save through the session so markers and the work area (which live
       in the session, not the node graph) are serialized too. options is a
       quercus-storage save bitmask; 0 means defaults. */
    {
        int32_t code = quercus_editor_save(
            session, (const uint8_t *)save_path, strlen(save_path), 0);

        if (code != 0) {
            fprintf(stderr, "FAILED: quercus_editor_save(\"%s\") returned %d\n",
                    save_path, code);
            quercus_project_release(project);
            quercus_editor_release(session);
            return 1;
        }
        printf("  saved project     %s\n", save_path);
    }

    /* 7. Release both refcounted handles. The project handle is independent
       of the session; releasing the session alone would not free it. */
    quercus_project_release(project);
    quercus_editor_release(session);
    printf("done\n");
    return 0;
}
