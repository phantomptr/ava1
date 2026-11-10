/* PS5 (FreeBSD 11 based): kern.arandom is the kernel CSPRNG, the source FreeBSD 11's
 * own arc4random uses. getentropy() arrived in FreeBSD 12. */
#include "ava1_platform.h"

#include <sys/types.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/sysctl.h>
#include <time.h>
#include <unistd.h>

/* Hidden behind __POSIX_VISIBLE in the SDK headers, as in runtime.c. */
extern int posix_fallocate(int fd, off_t offset, off_t len);
extern int posix_fadvise(int fd, off_t offset, off_t len, int advice);
#ifndef POSIX_FADV_DONTNEED
#define POSIX_FADV_DONTNEED 4
#endif

#ifndef KERN_ARND
#define KERN_ARND 37
#endif

int ava1_platform_random(uint8_t *buf, size_t n) {
    int mib[2] = { CTL_KERN, KERN_ARND };
    while (n > 0) {
        size_t want = n > 256 ? 256 : n, got = want;
        if (sysctl(mib, 2, buf, &got, NULL, 0) != 0 || got == 0) return -1;
        buf += got;
        n -= got;
    }
    return 0;
}

void ava1_platform_sleep_ms(unsigned ms) { usleep((useconds_t)ms * 1000u); }

int ava1_platform_preallocate(int fd, uint64_t len) {
    int rc = posix_fallocate(fd, 0, (off_t)len);
    if (rc == 0 || rc == ENOSPC) return rc;
    return ftruncate(fd, (off_t)len) == 0 ? 0 : errno; /* exFAT may refuse fallocate */
}

void ava1_platform_set_mtime(int fd, const char *path, uint64_t mtime) {
    struct timespec ts[2];
    (void)fd;
    ts[0].tv_sec = (time_t)mtime;
    ts[0].tv_nsec = 0;
    ts[1] = ts[0];
    (void)utimensat(AT_FDCWD, path, ts, 0);
}

/* Dropping cached pages (JF_READBACK reads from the drive). FreeBSD 11's POSIX_FADV_DONTNEED releases the
 * clean buffers but only deactivates their pages, which stay valid; msync(MS_INVALIDATE) then removes the
 * clean, unwired pages from the file's object. Neither call has been seen on this kernel before, and
 * FreeBSD kills a process that makes a system call the kernel lacks (SIGSYS): the first try runs with
 * SIGSYS ignored, so a missing call answers ENOSYS instead, and a missing one is never made again. */
static pthread_mutex_t g_drop_mu = PTHREAD_MUTEX_INITIALIZER;
static int g_drop_tried;  /* the first try is done (under g_drop_mu, then read atomically) */
static int g_no_fadvise, g_no_msync;

static int drop_fadvise(int fd, uint64_t off, uint64_t len) {
    int rc = posix_fadvise(fd, (off_t)off, (off_t)len, POSIX_FADV_DONTNEED);
    return rc == -1 ? errno : rc; /* the error itself, or -1 and errno (a missing call) */
}

static int drop_msync(int fd, uint64_t off, uint64_t len) {
    void *p = mmap(NULL, (size_t)len, PROT_READ, MAP_SHARED, fd, (off_t)off);
    int rc;
    if (p == MAP_FAILED) return errno;
    rc = msync(p, (size_t)len, MS_INVALIDATE) == 0 ? 0 : errno;
    (void)munmap(p, (size_t)len);
    return rc;
}

static int drop_both(int fd, uint64_t off, uint64_t len) {
    int a = __atomic_load_n(&g_no_fadvise, __ATOMIC_RELAXED) ? ENOSYS : drop_fadvise(fd, off, len);
    int b = __atomic_load_n(&g_no_msync, __ATOMIC_RELAXED) ? ENOSYS : drop_msync(fd, off, len);
    if (a == ENOSYS) __atomic_store_n(&g_no_fadvise, 1, __ATOMIC_RELAXED);
    if (b == ENOSYS) __atomic_store_n(&g_no_msync, 1, __ATOMIC_RELAXED);
    return b ? b : a; /* msync is what evicts: its answer counts first */
}

int ava1_platform_drop_cache(int fd, uint64_t off, uint64_t len, const char **how) {
    int rc;
    *how = "posix_fadvise(DONTNEED) + msync(MS_INVALIDATE)";
    if (!__atomic_load_n(&g_drop_tried, __ATOMIC_ACQUIRE)) {
        pthread_mutex_lock(&g_drop_mu);
        if (!g_drop_tried) {
            struct sigaction ign, old;
            memset(&ign, 0, sizeof ign);
            ign.sa_handler = SIG_IGN;
            sigemptyset(&ign.sa_mask);
            (void)sigaction(SIGSYS, &ign, &old);
            rc = drop_both(fd, off, len);
            (void)sigaction(SIGSYS, &old, NULL);
            __atomic_store_n(&g_drop_tried, 1, __ATOMIC_RELEASE);
            pthread_mutex_unlock(&g_drop_mu);
            return rc;
        }
        pthread_mutex_unlock(&g_drop_mu);
    }
    if (__atomic_load_n(&g_no_fadvise, __ATOMIC_RELAXED) && __atomic_load_n(&g_no_msync, __ATOMIC_RELAXED)) return ENOSYS;
    return drop_both(fd, off, len);
}

int ava1_platform_open_direct(const char *path) { return open(path, O_RDONLY | O_DIRECT | O_NOFOLLOW); }
