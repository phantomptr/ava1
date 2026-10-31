#include "ava1_thread.h"

#include <stdlib.h>
#include <time.h>

static void (*volatile g_start_hook)(void);

void ava1_thread_set_start_hook(void (*hook)(void)) { g_start_hook = hook; }

typedef struct {
    void *(*fn)(void *);
    void *arg;
} tramp_t;

static void *tramp_main(void *p) {
    tramp_t t = *(tramp_t *)p;
    void (*hook)(void) = g_start_hook;
    free(p);
    if (hook) hook();
    return t.fn(t.arg);
}

int ava1_thread_create(pthread_t *out, const pthread_attr_t *attr, void *(*fn)(void *), void *arg) {
    tramp_t *t;
    int rc;
    if (!g_start_hook) return pthread_create(out, attr, fn, arg);
    t = malloc(sizeof *t);
    /* No memory for the trampoline: the thread still starts, only without the hook. */
    if (!t) return pthread_create(out, attr, fn, arg);
    t->fn = fn;
    t->arg = arg;
    rc = pthread_create(out, attr, tramp_main, t);
    if (rc != 0) free(t);
    return rc;
}

int ava1_thread_start(void *(*fn)(void *), void *arg, pthread_t *out) {
    return ava1_thread_start_stack(fn, arg, out, AVA1_THREAD_STACK);
}

int ava1_thread_start_stack(void *(*fn)(void *), void *arg, pthread_t *out, size_t stack) {
    pthread_attr_t attr;
    pthread_t t;
    int rc;
    if (pthread_attr_init(&attr) != 0) return -1;
    if (pthread_attr_setstacksize(&attr, stack) != 0) {
        pthread_attr_destroy(&attr);
        return -1;
    }
    if (!out) (void)pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    rc = ava1_thread_create(out ? out : &t, &attr, fn, arg);
    pthread_attr_destroy(&attr);
    return rc == 0 ? 0 : -1;
}

uint64_t ava1_mono_us(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000u + (uint64_t)ts.tv_nsec / 1000u;
}

uint64_t ava1_mono_ms(void) { return ava1_mono_us() / 1000u; }
