/* CLN2-55: a legacy 0755 .hoh on a read-only mount, without requiring root.
 * This test-only LD_PRELOAD seam fails chmod/fchmod of exactly CLN2_STATE_DIR and writable opens under it.
 * Read-only access stays real. Never linked into or shipped with the product. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int state_dir(const char *path) {
    const char *dir = getenv("CLN2_STATE_DIR");
    return dir && strcmp(path, dir) == 0;
}

static int mode_error(void) {
    const char *value = getenv("CLN2_MODE_ERRNO");
    return value ? atoi(value) : EROFS;
}

int chmod(const char *path, mode_t mode) {
    if (state_dir(path)) { errno = mode_error(); return -1; }
    int (*real)(const char *, mode_t) = dlsym(RTLD_NEXT, "chmod");
    return real(path, mode);
}

int fchmod(int fd, mode_t mode) {
    char link[64], path[PATH_MAX];
    snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
    ssize_t n = readlink(link, path, sizeof(path) - 1);
    if (n >= 0) {
        path[n] = '\0';
        if (state_dir(path)) { errno = mode_error(); return -1; }
    }
    int (*real)(int, mode_t) = dlsym(RTLD_NEXT, "fchmod");
    return real(fd, mode);
}

static int readonly(const char *path, int flags) {
    if (getenv("CLN2_ALLOW_WRITES")) return 0;
    const char *dir = getenv("CLN2_STATE_DIR");
    size_t n = dir ? strlen(dir) : 0;
    return dir && strncmp(path, dir, n) == 0 && path[n] == '/' &&
           ((flags & O_ACCMODE) != O_RDONLY || (flags & O_CREAT));
}

#define OPEN(name) \
int name(const char *path, int flags, ...) { \
    mode_t mode = 0; \
    if (flags & O_CREAT) { va_list ap; va_start(ap, flags); mode = va_arg(ap, int); va_end(ap); } \
    if (readonly(path, flags)) { errno = EROFS; return -1; } \
    int (*real)(const char *, int, ...) = dlsym(RTLD_NEXT, #name); \
    return real(path, flags, mode); \
}
OPEN(open)
OPEN(open64)
