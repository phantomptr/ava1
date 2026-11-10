#if defined(__linux__) && !defined(_GNU_SOURCE)
#define _GNU_SOURCE /* glibc declares O_DIRECT only with it */
#endif
#include "ava1_platform.h"

#include <errno.h>
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <sys/random.h>
#endif

int ava1_platform_random(uint8_t *buf, size_t n) {
    while (n > 0) {
        size_t k = n > 256 ? 256 : n;
        if (getentropy(buf, k) != 0) return -1;
        buf += k;
        n -= k;
    }
    return 0;
}

void ava1_platform_sleep_ms(unsigned ms) { usleep((useconds_t)ms * 1000u); }

int ava1_platform_preallocate(int fd, uint64_t len) {
#if defined(__linux__)
    int rc = posix_fallocate(fd, 0, (off_t)len);
    if (rc == 0 || rc == ENOSPC) return rc;
#endif
    return ftruncate(fd, (off_t)len) == 0 ? 0 : errno;
}

void ava1_platform_set_mtime(int fd, const char *path, uint64_t mtime) {
    struct timespec ts[2];
    (void)path;
    ts[0].tv_sec = (time_t)mtime;
    ts[0].tv_nsec = 0;
    ts[1] = ts[0];
    (void)futimens(fd, ts);
}

#if defined(__FreeBSD__)
/* FreeBSD's POSIX_FADV_DONTNEED releases the clean buffers but only deactivates their pages, which stay
 * valid: a read right after is still answered from memory. msync(MS_INVALIDATE) then takes the clean,
 * now unwired pages out of the file's object. */
static int invalidate_mapped(int fd, uint64_t off, uint64_t len) {
    void *p = mmap(NULL, (size_t)len, PROT_READ, MAP_SHARED, fd, (off_t)off);
    int rc;
    if (p == MAP_FAILED) return errno;
    rc = msync(p, (size_t)len, MS_INVALIDATE) == 0 ? 0 : errno;
    (void)munmap(p, (size_t)len);
    return rc;
}
#endif

int ava1_platform_drop_cache(int fd, uint64_t off, uint64_t len, const char **how) {
#if defined(__linux__)
    *how = "posix_fadvise(DONTNEED)";
    return posix_fadvise(fd, (off_t)off, (off_t)len, POSIX_FADV_DONTNEED);
#elif defined(__FreeBSD__)
    int rc;
    *how = "posix_fadvise(DONTNEED) + msync(MS_INVALIDATE)";
    if ((rc = posix_fadvise(fd, (off_t)off, (off_t)len, POSIX_FADV_DONTNEED)) != 0) return rc;
    return invalidate_mapped(fd, off, len);
#else
    (void)fd;
    (void)off;
    (void)len;
    *how = "no call to drop cached pages on this OS";
    return ENOTSUP;
#endif
}

int ava1_platform_open_direct(const char *path) {
#ifdef O_DIRECT
    return open(path, O_RDONLY | O_DIRECT | O_NOFOLLOW);
#else
    (void)path;
    errno = ENOTSUP;
    return -1;
#endif
}
