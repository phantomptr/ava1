/* Threads and clocks (SPEC.md §15's 256 KiB rule). */
#ifndef AVA1_THREAD_H
#define AVA1_THREAD_H

#include <pthread.h>
#include <stddef.h>
#include <stdint.h>

#define AVA1_THREAD_STACK (256u * 1024u)

/* The management workers' stack (the old management thread had 512 KiB). */
#define AVA1_MGMT_STACK (512u * 1024u)

/* Starts fn(arg) on a 256 KiB stack. out == NULL: detached. 0 or -1. */
int ava1_thread_start(void *(*fn)(void *), void *arg, pthread_t *out);
/* The same on a stack of `stack` bytes. */
int ava1_thread_start_stack(void *(*fn)(void *), void *arg, pthread_t *out, size_t stack);
/* pthread_create for every thread the library starts: the start hook, when one is set, runs
 * first on the new thread (an embedder names its threads there). Returns what pthread_create does. */
int ava1_thread_create(pthread_t *out, const pthread_attr_t *attr, void *(*fn)(void *), void *arg);
/* Sets the hook (NULL: none). Set it once, before the library starts any thread. */
void ava1_thread_set_start_hook(void (*hook)(void));
uint64_t ava1_mono_ms(void);
uint64_t ava1_mono_us(void);

#endif
