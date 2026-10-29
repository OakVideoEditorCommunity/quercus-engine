/* Minimal C consumer: create an editor session and release it. */
#include <stdio.h>
#include "quercus_engine.h"

int main(void) {
	void *session = quercus_editor_new();
	if (session == NULL) {
		fprintf(stderr, "quercus_editor_new failed\n");
		return 1;
	}
	printf("quercus editor session created\n");
	quercus_editor_release(session);
	return 0;
}
