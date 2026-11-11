/*
 * Writes logs, spans and metric snapshots through the duckmon C ABI, flushes,
 * queries and closes.
 *
 * Build from the workspace root after cargo build --release:
 *   cc -std=c11 -Wall -Wextra -Werror -I crates/duckmon-ffi/include \
 *      crates/duckmon-ffi/examples/roundtrip.c -L target/release -lduckmon_ffi \
 *      -Wl,-rpath,$PWD/target/release -o /tmp/roundtrip_c
 *
 * Or against the static library, adding the system libraries that
 * cargo rustc -p duckmon-ffi --release --crate-type staticlib -- --print native-static-libs
 * lists (here for macOS):
 *   cc -std=c11 -I crates/duckmon-ffi/include crates/duckmon-ffi/examples/roundtrip.c \
 *      target/release/libduckmon_ffi.a -lc++ -lc -lm -liconv -framework CoreFoundation -o /tmp/roundtrip_c
 *
 * Run:
 *   /tmp/roundtrip_c <dir> <count> [arrow_ipc_file]
 *
 * It writes <count> events, one third of each kind, then runs a count query.
 * The query result, an Arrow IPC stream, is saved to arrow_ipc_file when given.
 */

#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "duckmon.h"

/* Events sent in one duckmon_store_write_events call. */
#define EVENTS_PER_WRITE 100

/* Room for one JSON line. Every line this program builds is far shorter. */
#define LINE_SIZE 512

static int fail(const char *what, int32_t code) {
    fprintf(stderr, "%s failed with code %" PRId32 ": %s\n", what, code, duckmon_last_error());
    return 1;
}

static uint64_t now_nanos(void) {
    struct timespec now;
    timespec_get(&now, TIME_UTC);
    return (uint64_t)now.tv_sec * 1000000000u + (uint64_t)now.tv_nsec;
}

/* Writes one JSON line for event number i to line. */
static void event_line(char *line, uint64_t i, uint64_t ts) {
    switch (i % 3) {
    case 0:
        snprintf(line, LINE_SIZE,
                 "{\"ts\":%" PRIu64 ",\"name\":\"app.http\",\"message\":\"handled request %" PRIu64
                 "\",\"attrs\":{\"level\":\"info\",\"source\":\"roundtrip.c\"}}\n",
                 ts, i);
        break;
    case 1:
        snprintf(line, LINE_SIZE,
                 "{\"ts\":%" PRIu64 ",\"name\":\"http.request\",\"duration_ns\":%" PRIu64
                 ",\"trace_id\":\"4bf92f3577b34da6a3ce929d0e0e4736\",\"span_id\":\"%016" PRIx64
                 "\",\"attrs\":{\"source\":\"roundtrip.c\"}}\n",
                 ts, i * 1000, i + 1);
        break;
    default:
        snprintf(line, LINE_SIZE,
                 "{\"ts\":%" PRIu64 ",\"name\":\"process.stats\",\"attrs\":{\"cpu.percent\":0.5,\"value\":%" PRIu64
                 ",\"source\":\"roundtrip.c\"}}\n",
                 ts, i);
        break;
    }
}

int main(int argc, char **argv) {
    if (argc != 3 && argc != 4) {
        fprintf(stderr, "usage: roundtrip_c <dir> <count> [arrow_ipc_file]\n");
        return 2;
    }
    const char *dir = argv[1];
    uint64_t count = strtoull(argv[2], NULL, 10);
    const char *ipc_path = argc == 4 ? argv[3] : NULL;

    if (duckmon_abi_version() != DUCKMON_ABI_VERSION) {
        fprintf(stderr, "library ABI version %" PRIu32 " differs from header version %d\n",
                duckmon_abi_version(), DUCKMON_ABI_VERSION);
        return 1;
    }

    duckmon_store *store = NULL;
    int32_t code = duckmon_open(dir, "{\"resource\":{\"service.name\":\"roundtrip_c\"}}", &store);
    if (code != DUCKMON_OK) {
        return fail("duckmon_open", code);
    }

    char *text = malloc((size_t)EVENTS_PER_WRITE * LINE_SIZE);
    if (text == NULL) {
        fprintf(stderr, "out of memory\n");
        duckmon_store_free(store);
        return 1;
    }
    uint64_t start = now_nanos();
    uint64_t i = 0;
    while (i < count) {
        size_t used = 0;
        for (int n = 0; n < EVENTS_PER_WRITE && i < count; n++, i++) {
            event_line(text + used, i, start - i * 1000000u);
            used += strlen(text + used);
        }
        code = duckmon_store_write_events(store, text);
        if (code != DUCKMON_OK) {
            free(text);
            duckmon_store_free(store);
            return fail("duckmon_store_write_events", code);
        }
    }
    free(text);

    code = duckmon_store_flush(store);
    if (code != DUCKMON_OK) {
        duckmon_store_free(store);
        return fail("duckmon_store_flush", code);
    }

    duckmon_bytes result = {NULL, 0};
    code = duckmon_store_query(store,
                               "SELECT count(*) AS rows, count(*) FILTER (WHERE duration_ns IS NOT NULL) AS spans "
                               "FROM events WHERE attrs->>'source' = 'roundtrip.c'",
                               &result);
    if (code != DUCKMON_OK) {
        duckmon_store_free(store);
        return fail("duckmon_store_query", code);
    }
    printf("wrote %" PRIu64 " events\n", count);
    printf("query returned %zu bytes of Arrow IPC\n", result.len);
    if (ipc_path != NULL) {
        FILE *file = fopen(ipc_path, "wb");
        if (file == NULL || fwrite(result.data, 1, result.len, file) != result.len || fclose(file) != 0) {
            fprintf(stderr, "could not write %s\n", ipc_path);
            duckmon_bytes_free(&result);
            duckmon_store_free(store);
            return 1;
        }
        printf("saved Arrow IPC to %s\n", ipc_path);
    }
    duckmon_bytes_free(&result);

    code = duckmon_store_close(store);
    duckmon_store_free(store);
    if (code != DUCKMON_OK) {
        return fail("duckmon_store_close", code);
    }
    printf("closed\n");
    return 0;
}
