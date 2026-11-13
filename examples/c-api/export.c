/* SPDX-License-Identifier: GPL-3.0-or-later */
/*
 * Quercus engine C ABI export example.
 *
 * Creates an editor session, creates a sequence, imports a media file,
 * places a clip on the first video track, exports the sequence through
 * quercus_task_export, and waits for the background task.
 *
 * Usage:
 *   export <media-file> [output.mp4] [seconds]
 *
 *   media-file   required; any file the engine can probe (e.g. an .mp4)
 *   output.mp4   optional output path (default: quercus-export.mp4)
 *   seconds      optional export length (default: 1)
 *
 * The exported "viewer" is the sequence node id itself. The C ABI has no
 * separate viewer object to create, so no viewer handle is needed. Task
 * states are not exposed as named constants in the generated header; the
 * values below come from the engine's public task API documentation
 * (0 running, 1 succeeded, 2 failed, 3 cancelled).
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "quercus_engine.h"

#define BUF_CAP 1024
#define EXPORT_VIDEO_WIDTH 640
#define EXPORT_VIDEO_HEIGHT 360
#define EXPORT_CLIP_FRAMES 25 /* duration of the placed clip, in frames */

enum {
    TASK_RUNNING = 0,
    TASK_SUCCEEDED = 1,
    TASK_FAILED = 2,
    TASK_CANCELLED = 3
};

static int fail(const char *step)
{
    fprintf(stderr, "FAILED: %s\n", step);
    return 1;
}

int main(int argc, char **argv)
{
    const char *media_path;
    const char *output_path;
    double seconds;
    const uint64_t invalid = quercus_node_id_invalid();
    void *session;
    void *project;
    uint64_t sequence;
    uint64_t footage;
    uint64_t clip;
    uint64_t task;
    QuercusEditResult edit_result;
    EncodingParams params;
    Rational fps;
    uint8_t error[512];

    if (argc < 2) {
        fprintf(stderr, "usage: %s <media-file> [output.mp4] [seconds]\n",
                argv[0]);
        return 2;
    }
    media_path = argv[1];
    output_path = argc > 2 ? argv[2] : "quercus-export.mp4";
    seconds = argc > 3 ? atof(argv[3]) : 1.0;
    if (seconds <= 0.0) {
        fprintf(stderr, "FAILED: export length must be positive\n");
        return 2;
    }

    printf("Quercus engine C ABI export example\n");

    session = quercus_editor_new();
    if (session == NULL) {
        return fail("quercus_editor_new");
    }
    project = quercus_editor_project(session);
    if (project == NULL) {
        quercus_editor_release(session);
        return fail("quercus_editor_project");
    }

    sequence = quercus_editor_create_sequence(
        session, (const uint8_t *)"Export", strlen("Export"));
    if (sequence == invalid) {
        quercus_project_release(project);
        quercus_editor_release(session);
        return fail("quercus_editor_create_sequence");
    }
    fps = quercus_sequence_frame_rate(project, sequence);
    printf("  sequence          %llu at %lld/%lld fps\n",
           (unsigned long long)sequence, (long long)fps.num,
           (long long)fps.den);

    footage = quercus_footage_import(project, (const uint8_t *)media_path,
                                     strlen(media_path));
    if (footage == invalid) {
        fprintf(stderr, "FAILED: quercus_footage_import(\"%s\")\n",
                media_path);
        quercus_project_release(project);
        quercus_editor_release(session);
        return 1;
    }
    printf("  footage           %llu (%s)\n",
           (unsigned long long)footage, media_path);

    /* Place the first 25 frames of the footage on video track 0. The edit
       params use frame timestamps (in_ts/out_ts), not seconds. */
    memset(&edit_result, 0, sizeof(edit_result));
    memset(&params, 0, sizeof(params));
    memset(error, 0, sizeof(error));
    {
        QuercusEditParams place;
        int32_t code;

        memset(&place, 0, sizeof(place));
        place.a = footage;
        place.kind = 0; /* 0 = video track (1 audio, 2 subtitle) */
        place.track_index = 0;
        place.in_ts = 0;
        place.out_ts = EXPORT_CLIP_FRAMES;
        place.media_in_ts = 0;

        code = quercus_editor_edit(session,
                                   QUERCUS_EDITOP_PLACEFOOTAGECLIP, &place,
                                   &edit_result, error, sizeof(error));
        if (code != 0) {
            fprintf(stderr, "FAILED: place clip returned %d: %s\n", code,
                    (const char *)error);
            quercus_project_release(project);
            quercus_editor_release(session);
            return 1;
        }
    }
    clip = edit_result.primary;
    if (clip == invalid) {
        quercus_project_release(project);
        quercus_editor_release(session);
        return fail("place clip returned an invalid clip id");
    }
    printf("  clip              %llu\n", (unsigned long long)clip);

    /* Encoding params are an explicit POD struct in the header; the ABI has
       no public "default encoding params" call for this task struct (the
       similarly named quercus_codec_encoding_params_default returns a
       different, encoder-side struct). Zero-initialize and fill the fields
       the export task reads. */
    params.format = QUERCUS_FORMAT_MPEG4VIDEO;
    params.video_enabled = true;
    params.video_codec = QUERCUS_CODEC_H264;
    params.video_width = EXPORT_VIDEO_WIDTH;
    params.video_height = EXPORT_VIDEO_HEIGHT;
    /* Frame *duration* rational: for 30000/1001 fps this is 1001/30000. */
    params.video_time_base_num = (int32_t)fps.den;
    params.video_time_base_den = (int32_t)fps.num;
    params.video_pixel_format = 0;
    params.audio_enabled = true;
    params.audio_codec = QUERCUS_CODEC_AAC;
    params.audio_sample_rate = 48000;
    params.audio_channel_layout = 3; /* ffmpeg AV_CH_LAYOUT_STEREO */
    params.subtitles_enabled = false;
    params.export_length_num = (int32_t)(seconds * 1000.0);
    params.export_length_den = 1000;
    params.has_custom_range = false;
    params.video_bit_rate = 0;
    params.audio_bit_rate = 0;
    params.color_override_enabled = false;

    task = quercus_task_export(project, sequence, (const uint8_t *)output_path,
                               strlen(output_path), &params);
    if (task == 0) {
        quercus_project_release(project);
        quercus_editor_release(session);
        return fail("quercus_task_export returned task id 0");
    }
    printf("  export task       %llu\n", (unsigned long long)task);

    /* quercus_task_wait blocks until the task reaches a terminal state and
       returns true only for success. For a UI, poll quercus_task_state and
       quercus_task_progress instead. */
    if (!quercus_task_wait(task)) {
        uint8_t message[BUF_CAP];
        int32_t len = quercus_task_error(task, message, sizeof(message));
        int32_t state = quercus_task_state(task);

        if (len >= 0) {
            printf("  task error        %s\n", (const char *)message);
        }
        fprintf(stderr, "FAILED: export task ended in state %d\n", state);
        quercus_task_release(task);
        quercus_project_release(project);
        quercus_editor_release(session);
        return 1;
    }
    printf("  task state        %d (succeeded)\n", quercus_task_state(task));
    printf("  wrote             %s\n", output_path);

    quercus_task_release(task);
    quercus_project_release(project);
    quercus_editor_release(session);
    printf("done\n");
    return 0;
}
