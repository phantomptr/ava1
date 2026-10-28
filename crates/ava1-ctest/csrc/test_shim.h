/* What an embedder adds to the core test shim (test_shim.c): a crate that builds this shim next to
 * its own C fills in `ava1_test_hooks` before it starts a server. Every hook may be NULL (the core
 * crate leaves them all NULL). */
#ifndef AVA1_TEST_SHIM_H
#define AVA1_TEST_SHIM_H

#include <stddef.h>
#include <stdint.h>

typedef struct {
    /* Methods other than node.info and the shim's own test methods (NULL: "unknown method"). */
    int (*rpc)(uint16_t method, const uint8_t *body, uint32_t len, uint8_t *out, size_t cap, size_t *out_len);
    /* Nonzero: the test server advertises AVA1_CAP_MGMT. */
    int (*mgmt_installed)(void);
    /* The data layer's refuse_link (nonzero: never follow `abs`). */
    int (*refuse_link)(const char *abs);
    /* After every data-layer start, once the operation registry is empty: register job.run operations
     * and reset the embedder's knobs. */
    void (*data_started)(void);
    /* ava1_test_data_knob names the core does not know: 0 = handled, -1 = unknown. */
    int (*knob)(const char *name, uint32_t v);
} ava1_test_hooks_t;

extern ava1_test_hooks_t ava1_test_hooks;

#endif
