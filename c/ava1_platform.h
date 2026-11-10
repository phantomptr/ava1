/* What AVA1 needs from the OS (SPEC.md §4). PS5: platform_ps5.c; host: platform_posix.c. */
#ifndef AVA1_PLATFORM_H
#define AVA1_PLATFORM_H

#include <stddef.h>
#include <stdint.h>

/* Fill buf from the OS's secure random source. 0 on success, -1 on failure. */
int ava1_platform_random(uint8_t *buf, size_t n);
void ava1_platform_sleep_ms(unsigned ms);
/* Reserve real blocks for a new file (ENOSPC early, no fragmentation); 0 or an errno. */
int ava1_platform_preallocate(int fd, uint64_t len);
/* Set a file's mtime (seconds). The PS5 uses utimensat on the path (verified in backup.c). */
void ava1_platform_set_mtime(int fd, const char *path, uint64_t mtime);
/* Best effort: drops the cached pages of bytes [off, off + len) of the file open at `fd` (written and
 * synced, so clean), so that the next read of them comes from the drive (JF_READBACK, SPEC.md §13.5).
 * `off` is page-aligned. 0 when the OS was told to, else an errno (ENOTSUP: this OS has no way; ENOSYS:
 * its kernel lacks the call). *how names the method, set either way, for a log line. */
int ava1_platform_drop_cache(int fd, uint64_t off, uint64_t len, const char **how);
/* Opens `path` read-only for direct I/O (O_DIRECT: past the page cache; reads in whole, aligned blocks).
 * A descriptor, or -1 with errno (ENOTSUP: no direct I/O on this OS; EINVAL: not on this filesystem). */
int ava1_platform_open_direct(const char *path);

#endif
