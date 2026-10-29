/* A complete AVA1 node in C: serves one folder, pairs new devices with a code, answers node.info.
 *
 *   make example-c
 *   build/ava1-node <state dir> <share dir> [port]
 *
 * <state dir> keeps the node's identity, the devices it trusts and its job journals; <share dir>
 * is the only place clients may write to or read from. The pairing window is open for two
 * minutes while no device is trusted yet; the code it prints is what a client types (the
 * `send_to_node` Rust example asks for it). */
#include <errno.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#include "ava1_data.h"
#include "ava1_events.h"
#include "ava1_gen.h"
#include "ava1_server.h"
#include "ava1_store.h"
#include "ava1_wire.h"

static char g_share[PATH_MAX];
static volatile sig_atomic_t g_stop;

/* Clients reach only the share folder (and what is under it). */
static int inside_share(const char *abs) {
    size_t n = strlen(g_share);
    return strncmp(abs, g_share, n) == 0 && (abs[n] == '\0' || abs[n] == '/');
}
static int may_write(const char *abs) { return inside_share(abs); }
static int may_read(const char *abs, int unsafe_read) { (void)unsafe_read; return inside_share(abs); }
static int same_device(const char *a, const char *b) {
    struct stat sa, sb;
    if (stat(a, &sa) != 0 || stat(b, &sb) != 0) return -1;
    return sa.st_dev == sb.st_dev;
}

static void on_pair_request(const char *peer, uint32_t code) {
    printf("pairing request from \"%s\": code %06u\n", peer, (unsigned)code);
    fflush(stdout);
}
static void on_log(const char *msg) { fprintf(stderr, "%s\n", msg); }

/* The data plane's methods (job.copy, job.status, ...) first; then node.info; nothing else. */
static int rpc(uint16_t method, const uint8_t *body, uint32_t len, uint8_t *out, size_t cap, size_t *out_len) {
    ava1_node_info_t ni;
    ava1_w_t w;
    int rc = ava1_data_rpc(method, body, len, out, cap, out_len);
    if (rc != -1) return rc;
    if (method != AVA1_METHOD_NODE_INFO) return AVA1_ERR_UNKNOWN_METHOD;
    memset(&ni, 0, sizeof ni);
    ni.version = (const uint8_t *)"1.0.0";
    ni.version_len = 5;
    ni.platform = (const uint8_t *)"c";
    ni.platform_len = 1;
    ni.name = (const uint8_t *)"example node";
    ni.name_len = 12;
    ava1_w_init(&w, out, cap);
    if (ava1_node_info_encode(&ni, &w) != 0) return AVA1_ERR_INTERNAL;
    *out_len = w.len;
    return AVA1_STATUS_OK;
}

static void stop(int sig) { (void)sig; g_stop = 1; }

int main(int argc, char **argv) {
    ava1_server_cfg_t cfg;
    ava1_data_cfg_t dc;
    char path[PATH_MAX + 32];
    int rc;
    if (argc < 3) {
        fprintf(stderr, "usage: %s <state dir> <share dir> [port]\n", argv[0]);
        return 2;
    }
    mkdir(argv[1], 0700);
    mkdir(argv[2], 0755);
    if (!realpath(argv[2], g_share)) {
        perror(argv[2]);
        return 1;
    }

    memset(&cfg, 0, sizeof cfg);
    cfg.port = argc > 3 ? (uint16_t)atoi(argv[3]) : (uint16_t)AVA1_DEFAULT_PORT;
    snprintf(path, sizeof path, "%s/identity", argv[1]);
    if (ava1_identity_load_or_create(path, &cfg.identity) != 0) {
        fprintf(stderr, "cannot read or create %s\n", path);
        return 1;
    }
    snprintf(cfg.name, sizeof cfg.name, "example node");
    snprintf(cfg.peers_path, sizeof cfg.peers_path, "%s/peers", argv[1]);
    cfg.ping_every_ms = 2000;
    cfg.dead_after_ms = 12000;
    cfg.handshake_ms = 10000;
    cfg.pairing_window_s = 120;
    cfg.on_pair_request = on_pair_request;
    cfg.log = on_log;
    cfg.rpc = rpc;

    memset(&dc, 0, sizeof dc);
    snprintf(dc.jobs_dir, sizeof dc.jobs_dir, "%s/jobs", argv[1]);
    mkdir(dc.jobs_dir, 0700);
    dc.may_write = may_write;
    dc.may_read = may_read;
    dc.same_device = same_device;
    snprintf(path, sizeof path, "%s/events.log", argv[1]);
    ava1_events_set_path(path);
    if ((rc = ava1_data_start(&dc)) != 0) {
        fprintf(stderr, "data layer: %s\n", strerror(-rc));
        return 1;
    }
    cfg.data = ava1_data_hooks();
    cfg.caps = AVA1_CAP_DATA_PLANE;

    if ((rc = ava1_server_start(&cfg)) != 0) {
        fprintf(stderr, "server: %s\n", strerror(-rc));
        return 1;
    }
    printf("listening on port %u, sharing %s\n", (unsigned)ava1_server_port(), g_share);
    fflush(stdout);
    signal(SIGINT, stop);
    signal(SIGTERM, stop);
    while (!g_stop) sleep(1);
    ava1_server_stop();
    ava1_data_stop();
    return 0;
}
