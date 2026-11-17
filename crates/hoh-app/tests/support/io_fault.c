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
 *   F7_IO_ARM      optional, a path: the first fault only while it exists (a running host's command, not its start)
 *   F7_CLEANUP_ERRNO  optional: after the first fault, the next pread of a self-test file fails once with this errno
 *                  (EINTR: SQLite retries it, and the errno of the failed call is gone when SQLite returns)
 *   F7_IO_NTH      optional: the first fault at the Nth matching call, not the first (default 1)
 *   F7_IO_PLANT    optional, a suffix: when the first fault fires, another creator makes the file named as the faulted
 *                  one with F7_IO_SUFFIX replaced by this suffix (O_EXCL), holding "FOREIGN-CREATOR-DATA\n"
 *   F7_UNLINK_ERRNO, F7_UNLINK_SUFFIX  optional: every unlink of a matching file whose path ends with the suffix fails
 *                  with that errno, from the first call on
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
static int seen;
static int cleanup_done;

/* Another creator's file: `path` with its last `cut` bytes replaced by `suffix`. */
static void plant(const char *path, size_t cut, const char *suffix) {
    static ssize_t (*real_write)(int, const void *, size_t);
    char name[4096];
    size_t keep = strlen(path) - cut;
    if (!real_write) {
        real_write = dlsym(RTLD_NEXT, "write");
    }
    if (keep + strlen(suffix) + 1 > sizeof name) {
        return;
    }
    memcpy(name, path, keep);
    strcpy(name + keep, suffix);
    int fd = open(name, O_CREAT | O_EXCL | O_WRONLY, 0644);
    if (fd >= 0) {
        const char data[] = "FOREIGN-CREATOR-DATA\n";
        if (real_write(fd, data, sizeof data - 1) < 0) {
            /* nothing to do: the file stands as another's either way */
        }
        close(fd);
    }
}

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
        } else if (!fired && (!getenv("F7_IO_PROBE") || getenv("HOH_WAL_PROBE_DB")) &&
                   (!getenv("F7_IO_ARM") || access(getenv("F7_IO_ARM"), F_OK) == 0)) {
            const char *kind = getenv("F7_IO_OP");
            const char *suffix = getenv("F7_IO_SUFFIX");
            size_t len = strlen(path);
            const char *nth = getenv("F7_IO_NTH");
            if (kind && suffix && strcmp(kind, op) == 0 && len >= strlen(suffix) &&
                strcmp(path + len - strlen(suffix), suffix) == 0 && ++seen >= (nth ? atoi(nth) : 1)) {
                fired = 1;
                result = atoi(err);
                if (getenv("F7_IO_PLANT")) {
                    plant(path, strlen(suffix), getenv("F7_IO_PLANT"));
                }
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

int unlink(const char *path) {
    REAL(int, "unlink", (const char *));
    const char *err = getenv("F7_UNLINK_ERRNO");
    const char *suffix = getenv("F7_UNLINK_SUFFIX");
    const char *match = getenv("F7_IO_MATCH");
    size_t len = strlen(path);
    if (err && suffix && strstr(path, match ? match : ".hoh-wal-selftest-") && len >= strlen(suffix) &&
        strcmp(path + len - strlen(suffix), suffix) == 0) {
        errno = atoi(err);
        return -1;
    }
    return real(path);
}
