/* Test artefact of crates/hoh-app/tests/db_faults.rs (docs/09 fault matrix item 7; ADR-016's note, round 2): an
 * LD_PRELOAD shim that fails I/O on the WAL self-test's files, after Codex's injector of the fault7 round-2 review.
 * The test builds it with `cc -shared -fPIC` into its own temporary directory and preloads it into the product's client,
 * host and probe; it is never built into, linked with or shipped by the product.
 *
 * Only files whose path contains F7_IO_MATCH (by default ".hoh-wal-selftest-", the self-test's) are touched; everything
 * else passes through.
 *   F7_IO_OP       pwrite | sync | truncate: the call that fails first (pwrite64; fsync or fdatasync; ftruncate64)
 *   F7_IO_SUFFIX   which file: the end of its path (".db", "-wal", "-journal", "-shm")
 *   F7_IO_ERRNO    the errno it fails with
 *   F7_IO_STATE    optional, a path: after the first fault it exists, and from then on every write, pwrite, sync and
 *                  truncate of a self-test file fails with the same errno, in every process (a file system that stays
 *                  full or over quota)
 *   F7_IO_PROBE    optional: the first fault only in the probe process (the one with HOH_WAL_PROBE_DB set)
 *   F7_CLEANUP_ERRNO  optional: after the first fault, the next pread of a self-test file fails once with this errno
 *                  (EINTR: SQLite retries it, and the errno of the failed call is gone when SQLite returns)
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>

static int fired;
static int cleanup_done;

static int selftest_file(int fd, char *path, size_t size) {
    char link[64];
    const char *match = getenv("F7_IO_MATCH");
    snprintf(link, sizeof link, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, size - 1);
    if (n < 0) {
        return 0;
    }
    path[n] = 0;
    return strstr(path, match ? match : ".hoh-wal-selftest-") != NULL;
}

/* The errno to fail `op` on `fd` with, or 0. errno is kept when the call passes through. */
static int fault(int fd, const char *op) {
    int saved = errno;
    char path[4096];
    const char *err = getenv("F7_IO_ERRNO");
    const char *state = getenv("F7_IO_STATE");
    int result = 0;
    if (err && selftest_file(fd, path, sizeof path)) {
        if (state && access(state, F_OK) == 0) {
            result = atoi(err);
        } else if (!fired && (!getenv("F7_IO_PROBE") || getenv("HOH_WAL_PROBE_DB"))) {
            const char *kind = getenv("F7_IO_OP");
            const char *suffix = getenv("F7_IO_SUFFIX");
            size_t len = strlen(path);
            if (kind && suffix && strcmp(kind, op) == 0 && len >= strlen(suffix) &&
                strcmp(path + len - strlen(suffix), suffix) == 0) {
                fired = 1;
                result = atoi(err);
                if (state) {
                    int made = open(state, O_CREAT | O_WRONLY, 0600);
                    if (made >= 0) {
                        close(made);
                    }
                }
            }
        }
    }
    errno = saved;
    return result;
}

#define REAL(type, name, args) \
    static type(*real) args;   \
    if (!real) {               \
        real = dlsym(RTLD_NEXT, name); \
    }

ssize_t pwrite64(int fd, const void *buf, size_t n, off64_t off) {
    REAL(ssize_t, "pwrite64", (int, const void *, size_t, off64_t));
    int e = fault(fd, "pwrite");
    if (e) {
        errno = e;
        return -1;
    }
    return real(fd, buf, n, off);
}

ssize_t write(int fd, const void *buf, size_t n) {
    REAL(ssize_t, "write", (int, const void *, size_t));
    int e = fault(fd, "write");
    if (e) {
        errno = e;
        return -1;
    }
    return real(fd, buf, n);
}

int fsync(int fd) {
    REAL(int, "fsync", (int));
    int e = fault(fd, "sync");
    if (e) {
        errno = e;
        return -1;
    }
    return real(fd);
}

int fdatasync(int fd) {
    REAL(int, "fdatasync", (int));
    int e = fault(fd, "sync");
    if (e) {
        errno = e;
        return -1;
    }
    return real(fd);
}

int ftruncate64(int fd, off64_t len) {
    REAL(int, "ftruncate64", (int, off64_t));
    int e = fault(fd, "truncate");
    if (e) {
        errno = e;
        return -1;
    }
    return real(fd, len);
}

ssize_t pread64(int fd, void *buf, size_t n, off64_t off) {
    REAL(ssize_t, "pread64", (int, void *, size_t, off64_t));
    const char *cleanup = getenv("F7_CLEANUP_ERRNO");
    char path[4096];
    if (fired && cleanup && !cleanup_done && selftest_file(fd, path, sizeof path)) {
        cleanup_done = 1;
        errno = atoi(cleanup);
        return -1;
    }
    return real(fd, buf, n, off);
}
