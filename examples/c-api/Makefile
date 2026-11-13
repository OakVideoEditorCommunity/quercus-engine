# SPDX-License-Identifier: GPL-3.0-or-later
#
# Local macOS build for the Quercus engine C ABI examples.
#
#   make            build quickstart, export and the worker launcher
#   make run-quickstart
#   make run-export
#
# Binaries go to $(BIN_DIR) (default /tmp) so the source tree stays clean.

CC := clang
ROOT := ../..
INCLUDE := $(ROOT)/crates/quercus-engine/include
LIBDIR := $(abspath $(ROOT)/target/debug)
FFMPEG_LIB := $(abspath $(ROOT)/.cache/ffmpeg/lib)
BIN_DIR ?= /tmp

CFLAGS ?= -std=c11 -Wall -Wextra -I$(INCLUDE)
LDLIBS := -L$(LIBDIR) -lquercus_engine -Wl,-rpath,$(LIBDIR)

MEDIA ?= $(ROOT)/testdata/demo.mp4
SAVE ?= $(BIN_DIR)/quercus-quickstart.ove
EXPORT_OUT ?= $(BIN_DIR)/quercus-export.mp4

QUICKSTART := $(BIN_DIR)/quercus-quickstart
EXPORT := $(BIN_DIR)/quercus-export
WORKER := $(BIN_DIR)/quercus-worker

.PHONY: all clean run-quickstart run-export

all: $(QUICKSTART) $(EXPORT) $(WORKER)

$(QUICKSTART): quickstart.c
	$(CC) $(CFLAGS) $< $(LDLIBS) -o $@

$(EXPORT): export.c
	$(CC) $(CFLAGS) $< $(LDLIBS) -o $@

$(WORKER): worker.c
	$(CC) $(CFLAGS) $< $(LDLIBS) -o $@

run-quickstart: $(QUICKSTART)
	DYLD_LIBRARY_PATH=$(FFMPEG_LIB) $(QUICKSTART) $(MEDIA) $(SAVE)

run-export: $(EXPORT) $(WORKER)
	QUERCUS_WORKER_BIN=$(WORKER) DYLD_LIBRARY_PATH=$(FFMPEG_LIB) \
		$(EXPORT) $(MEDIA) $(EXPORT_OUT) 1

clean:
	rm -f $(QUICKSTART) $(EXPORT) $(WORKER)
