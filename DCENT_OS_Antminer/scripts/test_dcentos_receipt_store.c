/* SPDX-License-Identifier: GPL-3.0-or-later */
#define _GNU_SOURCE
#define _XOPEN_SOURCE 700
#include "receipt_store.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <ftw.h>
#include <inttypes.h>
#include <limits.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/un.h>
#include <unistd.h>

#define TX_ID "Tx-1"
#define CLAIM_ID "Claim-1"

struct test_layout {
    char root[PATH_MAX];
    struct stat lock_stat;
    struct stat owner_stat;
    struct stat ledger_stat;
    char owner_sha[65];
};

static unsigned int assertions;
static int hook_mode;
static bool hook_fired;

static void require(bool condition, const char *label)
{
    ++assertions;
    if (!condition) {
        fprintf(stderr, "FAIL: %s\n", label);
        exit(1);
    }
}

static size_t checked_snprintf(char *buffer, size_t capacity,
                               const char *format, ...)
{
    va_list arguments;
    int length;

    va_start(arguments, format);
    length = vsnprintf(buffer, capacity, format, arguments);
    va_end(arguments);
    require(length >= 0 && (size_t)length < capacity, "fixture text fits");
    return (size_t)length;
}

static void bytes_to_hex(const unsigned char *bytes, char hex[65])
{
    static const char digits[] = "0123456789abcdef";
    size_t index;

    for (index = 0U; index < DCENT_RECEIPT_SHA256_BYTES; ++index) {
        hex[index * 2U] = digits[bytes[index] >> 4];
        hex[index * 2U + 1U] = digits[bytes[index] & 0x0fU];
    }
    hex[64] = '\0';
}

static void digest_hex(const void *data, size_t size, char hex[65])
{
    unsigned char digest[DCENT_RECEIPT_SHA256_BYTES];

    dcent_receipt_sha256(digest, data, size);
    bytes_to_hex(digest, hex);
}

static void path_join(char *out, size_t capacity, const char *left,
                      const char *right)
{
    (void)checked_snprintf(out, capacity, "%s/%s", left, right);
}

static void write_all(int fd, const void *data, size_t size)
{
    const unsigned char *cursor = data;

    while (size != 0U) {
        ssize_t written = write(fd, cursor, size);

        if (written < 0 && errno == EINTR)
            continue;
        require(written > 0, "fixture write completes");
        cursor += (size_t)written;
        size -= (size_t)written;
    }
}

static void write_file(const char *path, const void *data, size_t size)
{
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);

    require(fd >= 0, "fixture receipt opens");
    require(fchmod(fd, 0600) == 0, "fixture receipt mode is exact");
    write_all(fd, data, size);
    require(close(fd) == 0, "fixture receipt closes");
}

static size_t read_file_at(int directory_fd, const char *name,
                           unsigned char buffer[DCENT_RECEIPT_MAX_FILE],
                           int *retained_fd)
{
    struct stat metadata;
    size_t size = 0U;
    int fd = openat(directory_fd, name,
                    O_RDONLY | O_NOCTTY | O_NOFOLLOW | O_CLOEXEC);

    require(fd >= 0, "race source opens by descriptor");
    require(fstat(fd, &metadata) == 0 && S_ISREG(metadata.st_mode) &&
                metadata.st_size >= 1 &&
                (uintmax_t)metadata.st_size <= DCENT_RECEIPT_MAX_FILE,
            "race source is a bounded regular receipt");
    while (size < (size_t)metadata.st_size) {
        ssize_t received = read(fd, buffer + size,
                                (size_t)metadata.st_size - size);

        if (received < 0 && errno == EINTR)
            continue;
        require(received > 0, "race source read completes");
        size += (size_t)received;
    }
    require(retained_fd != NULL, "race source descriptor owner is provided");
    *retained_fd = fd;
    return size;
}

static int create_unix_socket(const char *path)
{
    struct sockaddr_un address;
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);

    require(fd >= 0, "UNIX socket descriptor opens");
    memset(&address, 0, sizeof(address));
    address.sun_family = AF_UNIX;
    (void)checked_snprintf(address.sun_path, sizeof(address.sun_path), "%s",
                           path);
    require(bind(fd, (const struct sockaddr *)&address, sizeof(address)) == 0,
            "UNIX socket pathname binds");
    require(chmod(path, 0600) == 0, "UNIX socket mode is exact");
    return fd;
}

static void digest_file(const char *path, char hex[65])
{
    unsigned char buffer[DCENT_RECEIPT_MAX_FILE];
    size_t size = 0U;
    int fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC);

    require(fd >= 0, "fixture receipt opens for digest");
    while (size < sizeof(buffer)) {
        ssize_t received = read(fd, buffer + size, sizeof(buffer) - size);

        if (received < 0 && errno == EINTR)
            continue;
        require(received >= 0, "fixture receipt reads for digest");
        if (received == 0)
            break;
        size += (size_t)received;
    }
    require(close(fd) == 0, "fixture digest descriptor closes");
    digest_hex(buffer, size, hex);
}

static size_t build_owner(char *buffer, size_t capacity, unsigned int pid)
{
    return checked_snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-lock-v3\n"
        "transaction_id=" TX_ID "\n"
        "boot_id=abcdef12-3456-7890-abcd-ef1234567890\n"
        "pid=%u\n"
        "starttime=456\n"
        "owner=zynq-sysupgrade\n",
        pid);
}

static size_t build_binding(const struct test_layout *layout, char *buffer,
                            size_t capacity, uintmax_t ledger_device,
                            uintmax_t ledger_inode, unsigned int owner_pid)
{
    return checked_snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-resource-ledger-abi1\n"
        "transaction_id=" TX_ID "\n"
        "boot_id=abcdef12-3456-7890-abcd-ef1234567890\n"
        "owner_pid=%u\n"
        "owner_starttime=456\n"
        "owner_mount_namespace=1:22\n"
        "acquisition_guard_device_inode=9:10\n"
        "transaction_lock_path=/run/dcentos-sysupgrade.lock\n"
        "transaction_lock_device_inode=%" PRIuMAX ":%" PRIuMAX "\n"
        "transaction_lock_owner_device_inode=%" PRIuMAX ":%" PRIuMAX "\n"
        "transaction_lock_owner_sha256=%s\n"
        "storage_mount_id=7\n"
        "ledger_path=/run/dcentos-sysupgrade.lock/ledger\n"
        "ledger_device_inode=%" PRIuMAX ":%" PRIuMAX "\n"
        "owner=zynq-sysupgrade\n",
        owner_pid, (uintmax_t)layout->lock_stat.st_dev,
        (uintmax_t)layout->lock_stat.st_ino,
        (uintmax_t)layout->owner_stat.st_dev,
        (uintmax_t)layout->owner_stat.st_ino, layout->owner_sha,
        ledger_device, ledger_inode);
}

static size_t build_resource_intent(char *buffer, size_t capacity,
                                    const char *binding_sha,
                                    const char *resource_id)
{
    static const char body[] = "observed=true\n";
    char evidence_sha[65];
    int header;

    digest_hex(body, sizeof(body) - 1U, evidence_sha);
    header = snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-resource-intent-abi1\n"
        "binding_sha256=%s\n"
        "transaction_id=" TX_ID "\n"
        "kind=attachment\n"
        "resource_id=%s\n"
        "provenance=created\n"
        "identity_a=7\n"
        "identity_b=1\n"
        "identity_c=-\n"
        "evidence_type=attachment-intent-v1\n"
        "evidence_size=%zu\n"
        "evidence_sha256=%s\n\n",
        binding_sha, resource_id, sizeof(body) - 1U, evidence_sha);
    require(header >= 0 &&
                (size_t)header + sizeof(body) - 1U < capacity,
            "resource intent fits");
    memcpy(buffer + header, body, sizeof(body) - 1U);
    return (size_t)header + sizeof(body) - 1U;
}

static size_t build_resource_status(char *buffer, size_t capacity,
                                    const char *binding_sha,
                                    const char *intent_sha,
                                    const char *resource_id)
{
    return checked_snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-resource-status-abi1\n"
        "binding_sha256=%s\n"
        "transaction_id=" TX_ID "\n"
        "kind=attachment\n"
        "resource_id=%s\n"
        "intent_sha256=%s\n"
        "phase=pending\n"
        "revision=1\n"
        "ledger_generation=1\n"
        "previous_status_sha256=-\n"
        "actor_kind=owner\n"
        "actor_id=" TX_ID "\n"
        "evidence_type=-\n"
        "evidence_size=0\n"
        "evidence_sha256=-\n\n",
        binding_sha, resource_id, intent_sha);
}

static size_t build_claim_intent(char *buffer, size_t capacity,
                                 const char *binding_sha)
{
    static const char body[] = "owner_dead=true\n";
    char evidence_sha[65];
    int header;

    digest_hex(body, sizeof(body) - 1U, evidence_sha);
    header = snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-reconcile-intent-abi1\n"
        "binding_sha256=%s\n"
        "transaction_id=" TX_ID "\n"
        "claim_id=" CLAIM_ID "\n"
        "reconciler_boot_id=abcdef12-3456-7890-abcd-ef1234567890\n"
        "reconciler_pid=321\n"
        "reconciler_starttime=987654\n"
        "reconciler_mount_namespace=3:77\n"
        "maintenance_lock_path=/run/dcentos-maintenance.lock\n"
        "maintenance_lock_device_inode=4:88\n"
        "owner=zynq-sysupgrade-reconciler\n"
        "evidence_type=owner-death-v1\n"
        "evidence_size=%zu\n"
        "evidence_sha256=%s\n\n",
        binding_sha, sizeof(body) - 1U, evidence_sha);
    require(header >= 0 &&
                (size_t)header + sizeof(body) - 1U < capacity,
            "claim intent fits");
    memcpy(buffer + header, body, sizeof(body) - 1U);
    return (size_t)header + sizeof(body) - 1U;
}

static size_t build_claim_status(char *buffer, size_t capacity,
                                 const char *intent_sha)
{
    return checked_snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-reconcile-status-abi1\n"
        "claim_intent_sha256=%s\n"
        "phase=claimed\n"
        "revision=1\n"
        "ledger_generation=1\n"
        "previous_status_sha256=-\n"
        "actor_id=" CLAIM_ID "\n"
        "quiescence_sha256=-\n"
        "evidence_type=-\n"
        "evidence_size=0\n"
        "evidence_sha256=-\n\n",
        intent_sha);
}

static void create_resource(const struct test_layout *layout,
                            const char *binding_sha, const char *resource_id)
{
    char resources[PATH_MAX];
    char directory[PATH_MAX];
    char path[PATH_MAX];
    char buffer[DCENT_RECEIPT_MAX_FILE];
    char intent_sha[65];
    size_t size;

    path_join(resources, sizeof(resources), layout->root, "ledger/resources");
    (void)checked_snprintf(directory, sizeof(directory),
                           "%s/attachment--%s", resources, resource_id);
    require(mkdir(directory, 0700) == 0, "resource directory created");
    require(chmod(directory, 0700) == 0, "resource directory mode exact");
    size = build_resource_intent(buffer, sizeof(buffer), binding_sha,
                                 resource_id);
    digest_hex(buffer, size, intent_sha);
    path_join(path, sizeof(path), directory, "intent");
    write_file(path, buffer, size);
    size = build_resource_status(buffer, sizeof(buffer), binding_sha,
                                 intent_sha, resource_id);
    path_join(path, sizeof(path), directory, "status.1");
    write_file(path, buffer, size);
}

static void create_claim(const struct test_layout *layout,
                         const char *binding_sha)
{
    char directory[PATH_MAX];
    char path[PATH_MAX];
    char buffer[DCENT_RECEIPT_MAX_FILE];
    char intent_sha[65];
    size_t size;

    path_join(directory, sizeof(directory), layout->root,
              "ledger/reconcile.claim");
    require(mkdir(directory, 0700) == 0, "claim directory created");
    require(chmod(directory, 0700) == 0, "claim directory mode exact");
    size = build_claim_intent(buffer, sizeof(buffer), binding_sha);
    digest_hex(buffer, size, intent_sha);
    path_join(path, sizeof(path), directory, "intent");
    write_file(path, buffer, size);
    size = build_claim_status(buffer, sizeof(buffer), intent_sha);
    path_join(path, sizeof(path), directory, "status.1");
    write_file(path, buffer, size);
}

static void create_layout(struct test_layout *layout, const char *resource_id,
                          bool with_claim)
{
    char template[PATH_MAX];
    char path[PATH_MAX];
    char buffer[DCENT_RECEIPT_MAX_FILE];
    char binding_sha[65];
    size_t size;
    const char *temporary_root = getenv("TMPDIR");

    memset(layout, 0, sizeof(*layout));
    if (temporary_root == NULL || temporary_root[0] == '\0')
        temporary_root = "/tmp";
    (void)checked_snprintf(template, sizeof(template),
                           "%s/dcentos-receipt-store.XXXXXX", temporary_root);
    require(mkdtemp(template) != NULL, "temporary lock directory created");
    (void)checked_snprintf(layout->root, sizeof(layout->root), "%s", template);
    require(chmod(layout->root, 0700) == 0, "lock directory mode exact");
    path_join(path, sizeof(path), layout->root, "ledger");
    require(mkdir(path, 0700) == 0, "ledger directory created");
    require(chmod(path, 0700) == 0, "ledger directory mode exact");
    path_join(path, sizeof(path), layout->root, "ledger/resources");
    require(mkdir(path, 0700) == 0, "resources directory created");
    require(chmod(path, 0700) == 0, "resources directory mode exact");
    require(stat(layout->root, &layout->lock_stat) == 0,
            "lock identity captured");
    path_join(path, sizeof(path), layout->root, "ledger");
    require(stat(path, &layout->ledger_stat) == 0,
            "ledger identity captured");
    size = build_owner(buffer, sizeof(buffer), 123U);
    digest_hex(buffer, size, layout->owner_sha);
    path_join(path, sizeof(path), layout->root, "owner");
    write_file(path, buffer, size);
    require(stat(path, &layout->owner_stat) == 0,
            "owner identity captured");
    size = build_binding(layout, buffer, sizeof(buffer),
                         (uintmax_t)layout->ledger_stat.st_dev,
                         (uintmax_t)layout->ledger_stat.st_ino, 123U);
    digest_hex(buffer, size, binding_sha);
    path_join(path, sizeof(path), layout->root, "ledger/binding");
    write_file(path, buffer, size);
    if (resource_id != NULL)
        create_resource(layout, binding_sha, resource_id);
    if (with_claim)
        create_claim(layout, binding_sha);
}

static int remove_callback(const char *path, const struct stat *metadata,
                           int type, struct FTW *walk)
{
    (void)metadata;
    (void)walk;
    return type == FTW_DP ? rmdir(path) : unlink(path);
}

static void destroy_layout(const struct test_layout *layout)
{
    require(nftw(layout->root, remove_callback, 32, FTW_DEPTH | FTW_PHYS) == 0,
            "temporary layout removed");
}

static enum dcent_receipt_store_result scan_layout(
    const struct test_layout *layout, struct dcent_receipt_ledger_snapshot *out,
    struct dcent_receipt_store_error *error)
{
    enum dcent_receipt_store_result result;
    int fd = open(layout->root,
                  O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    off_t offset_before;
    off_t offset_after;

    require(fd >= 0, "lock descriptor opens");
    offset_before = lseek(fd, 0, SEEK_CUR);
    require(offset_before >= 0, "caller directory offset is observable");
    result = dcent_receipt_store_scan_forensic_abi1(fd, out, error);
    offset_after = lseek(fd, 0, SEEK_CUR);
    require(offset_after == offset_before,
            "scanner does not consume caller directory offset");
    require(fcntl(fd, F_GETFD) >= 0, "caller retains its lock descriptor");
    require(close(fd) == 0, "caller lock descriptor closes");
    return result;
}

static void expect_result(const struct test_layout *layout,
                          enum dcent_receipt_store_result expected,
                          const char *label)
{
    struct dcent_receipt_ledger_snapshot snapshot;
    struct dcent_receipt_ledger_snapshot sentinel;
    struct dcent_receipt_store_error error;
    enum dcent_receipt_store_result result;

    memset(&sentinel, 0xa5, sizeof(sentinel));
    snapshot = sentinel;
    result = scan_layout(layout, &snapshot, &error);
    if (result != expected)
        fprintf(stderr, "expected=%s actual=%s context=%s errno=%d format=%d\n",
                dcent_receipt_store_result_name(expected),
                dcent_receipt_store_result_name(result), error.context,
                error.system_errno, error.format_result);
    require(result == expected, label);
    if (expected == DCENT_RECEIPT_STORE_OK) {
        require(error.result == DCENT_RECEIPT_STORE_OK,
                "successful scan clears diagnostics");
    } else {
        require(memcmp(&snapshot, &sentinel, sizeof(snapshot)) == 0,
                "failed scan leaves output untouched");
        require(error.result == result, "failure diagnostic class is exact");
    }
}

static size_t open_fd_count(void)
{
    DIR *directory = opendir("/proc/self/fd");
    struct dirent *entry;
    size_t count = 0U;

    require(directory != NULL, "fd directory opens");
    while ((entry = readdir(directory)) != NULL) {
        if (strcmp(entry->d_name, ".") != 0 &&
            strcmp(entry->d_name, "..") != 0)
            ++count;
    }
    require(closedir(directory) == 0, "fd directory closes");
    return count;
}

static void rewrite_binding(const struct test_layout *layout,
                            uintmax_t ledger_device, uintmax_t ledger_inode,
                            unsigned int owner_pid)
{
    char path[PATH_MAX];
    char buffer[DCENT_RECEIPT_MAX_FILE];
    size_t size = build_binding(layout, buffer, sizeof(buffer), ledger_device,
                                ledger_inode, owner_pid);

    path_join(path, sizeof(path), layout->root, "ledger/binding");
    write_file(path, buffer, size);
}

void dcent_receipt_store_test_hook(
    enum dcent_receipt_store_test_point point, int directory_fd,
    const char *name)
{
    if (hook_fired)
        return;
    if (hook_mode == 1 && point == DCENT_RECEIPT_STORE_TEST_BEFORE_OPEN &&
        name != NULL && strcmp(name, "binding") == 0) {
        require(unlinkat(directory_fd, name, 0) == 0,
                "race hook removes binding");
        require(symlinkat("owner", directory_fd, name) == 0,
                "race hook substitutes symlink");
        hook_fired = true;
    } else if (hook_mode == 2 &&
               point == DCENT_RECEIPT_STORE_TEST_BETWEEN_PASSES) {
        char buffer[512];
        size_t size = build_owner(buffer, sizeof(buffer), 124U);
        int fd = openat(directory_fd, "owner",
                        O_WRONLY | O_TRUNC | O_NOFOLLOW | O_CLOEXEC);

        require(fd >= 0, "between-pass owner opens");
        write_all(fd, buffer, size);
        require(close(fd) == 0, "between-pass owner closes");
        hook_fired = true;
    } else if (point == DCENT_RECEIPT_STORE_TEST_BEFORE_OPEN && name != NULL &&
               strcmp(name, "binding") == 0 &&
               (hook_mode == 3 || hook_mode == 4 || hook_mode == 5 ||
                hook_mode == 6)) {
        unsigned char original[DCENT_RECEIPT_MAX_FILE];
        size_t original_size = 0U;
        int original_fd = -1;

        if (hook_mode == 3)
            original_size = read_file_at(directory_fd, name, original,
                                         &original_fd);
        require(unlinkat(directory_fd, name, 0) == 0,
                "race hook removes regular binding");
        if (hook_mode == 3) {
            int fd = openat(directory_fd, name,
                            O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0600);

            require(fd >= 0, "race hook installs new regular inode");
            write_all(fd, original, original_size);
            require(close(fd) == 0, "race regular inode closes");
            require(close(original_fd) == 0,
                    "unlinked race source descriptor closes");
        } else if (hook_mode == 4) {
            require(mkfifoat(directory_fd, name, 0600) == 0,
                    "race hook installs FIFO");
        } else if (hook_mode == 5) {
            require(mkdirat(directory_fd, name, 0700) == 0,
                    "race hook installs directory");
        } else {
            char path[sizeof(((struct sockaddr_un *)0)->sun_path)];
            int socket_fd;

            (void)checked_snprintf(path, sizeof(path),
                                   "/proc/self/fd/%d/%s", directory_fd, name);
            socket_fd = create_unix_socket(path);
            require(close(socket_fd) == 0,
                    "raced UNIX socket descriptor closes");
        }
        hook_fired = true;
    } else if (hook_mode == 7 &&
               point == DCENT_RECEIPT_STORE_TEST_BEFORE_OPEN && name != NULL &&
               strcmp(name, "resources") == 0) {
        int original_fd = openat(directory_fd, name,
                                 O_RDONLY | O_DIRECTORY | O_NOFOLLOW |
                                     O_CLOEXEC);

        require(original_fd >= 0,
                "race hook retains original resources directory");
        require(unlinkat(directory_fd, name, AT_REMOVEDIR) == 0,
                "race hook removes empty resources directory");
        require(mkdirat(directory_fd, name, 0700) == 0,
                "race hook installs same-shape resources directory");
        require(close(original_fd) == 0,
                "unlinked resources directory descriptor closes");
        hook_fired = true;
    }
}

static void test_canonical_layouts(void)
{
    struct test_layout layout;
    struct dcent_receipt_ledger_snapshot snapshot;
    struct dcent_receipt_store_error error;

    create_layout(&layout, NULL, false);
    expect_result(&layout, DCENT_RECEIPT_STORE_OK,
                  "zero-resource canonical ledger scans");
    require(scan_layout(&layout, &snapshot, NULL) == DCENT_RECEIPT_STORE_OK,
            "successful scan permits optional NULL diagnostics");
    require(scan_layout(&layout, &snapshot, &error) == DCENT_RECEIPT_STORE_OK &&
                snapshot.resource_count == 0U && !snapshot.claim_present &&
                snapshot.aggregate_bytes > 0U && snapshot.lock.pid == 123U &&
                snapshot.binding.ledger_device_inode.device ==
                    (uint64_t)layout.ledger_stat.st_dev &&
                snapshot.binding.ledger_device_inode.inode ==
                    (uint64_t)layout.ledger_stat.st_ino,
            "zero-resource snapshot is complete and owned");
    destroy_layout(&layout);

    create_layout(&layout, "ubi-1", true);
    require(scan_layout(&layout, &snapshot, &error) == DCENT_RECEIPT_STORE_OK &&
                snapshot.resource_count == 1U && snapshot.claim_present &&
                snapshot.resources[0].latest_phase ==
                    DCENT_RECEIPT_RESOURCE_PENDING &&
                snapshot.claim.latest_phase == DCENT_RECEIPT_CLAIM_CLAIMED,
            "resource and claim chains scan end to end");
    destroy_layout(&layout);

    create_layout(&layout, "ubi--1", false);
    require(scan_layout(&layout, &snapshot, &error) == DCENT_RECEIPT_STORE_OK &&
                snapshot.resource_count == 1U,
            "resource IDs containing double hyphens are not split ambiguously");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    {
        char path[PATH_MAX];
        char binding_sha[65];
        char resource_id[16];
        unsigned int index;

        path_join(path, sizeof(path), layout.root, "ledger/binding");
        digest_file(path, binding_sha);
        for (index = 0U; index < DCENT_RECEIPT_MAX_RESOURCES; ++index) {
            unsigned int permuted =
                (index * 17U + 7U) % DCENT_RECEIPT_MAX_RESOURCES;

            (void)checked_snprintf(resource_id, sizeof(resource_id), "r%02u",
                                   permuted);
            create_resource(&layout, binding_sha, resource_id);
        }
    }
    require(scan_layout(&layout, &snapshot, &error) == DCENT_RECEIPT_STORE_OK &&
                snapshot.resource_count == DCENT_RECEIPT_MAX_RESOURCES,
            "exact 32-resource ledger scans without readdir-order dependence");
    {
        unsigned int index;

        for (index = 0U; index < DCENT_RECEIPT_MAX_RESOURCES; ++index) {
            char resource_id[16];
            size_t resource_id_size = checked_snprintf(
                resource_id, sizeof(resource_id), "r%02u", index);

            require(snapshot.resources[index].resource_id.size ==
                            resource_id_size &&
                        memcmp(snapshot.resources[index].resource_id.bytes,
                               resource_id, resource_id_size) == 0,
                    "resource snapshot order is canonical lexical order");
        }
    }
    destroy_layout(&layout);
}

static void test_binding_and_topology_refusals(void)
{
    struct test_layout layout;
    char path[PATH_MAX];
    char other[PATH_MAX];
    int fd;
    unsigned int index;

    create_layout(&layout, NULL, false);
    rewrite_binding(&layout, (uintmax_t)layout.ledger_stat.st_dev,
                    (uintmax_t)layout.ledger_stat.st_ino + 1U, 123U);
    expect_result(&layout, DCENT_RECEIPT_STORE_BINDING_MISMATCH,
                  "binding cannot substitute another ledger inode");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    rewrite_binding(&layout, (uintmax_t)layout.ledger_stat.st_dev,
                    (uintmax_t)layout.ledger_stat.st_ino, 124U);
    expect_result(&layout, DCENT_RECEIPT_STORE_BINDING_MISMATCH,
                  "binding owner must match the lock-v3 owner anchor");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, ".operation");
    write_file(path, "x", 1U);
    expect_result(&layout, DCENT_RECEIPT_STORE_LAYOUT,
                  "private operation marker blocks clean inspection");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, "ledger/binding");
    require(unlink(path) == 0, "binding removed for missing-entry fixture");
    expect_result(&layout, DCENT_RECEIPT_STORE_LAYOUT,
                  "missing binding blocks inspection");
    destroy_layout(&layout);

    create_layout(&layout, "ubi-1", false);
    path_join(path, sizeof(path), layout.root,
              "ledger/resources/attachment--ubi-1/status.1");
    path_join(other, sizeof(other), layout.root,
              "ledger/resources/attachment--ubi-1/status.2");
    require(rename(path, other) == 0, "status gap fixture renamed");
    expect_result(&layout, DCENT_RECEIPT_STORE_LAYOUT,
                  "noncontiguous status revisions block inspection");
    destroy_layout(&layout);

    create_layout(&layout, "ubi-1", false);
    path_join(path, sizeof(path), layout.root,
              "ledger/resources/attachment--ubi-1");
    path_join(other, sizeof(other), layout.root,
              "ledger/resources/attachment--different");
    require(rename(path, other) == 0, "resource-name mismatch fixture renamed");
    expect_result(&layout, DCENT_RECEIPT_STORE_BINDING_MISMATCH,
                  "resource directory name authenticates intent kind and ID");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    for (index = 0U; index < 33U; ++index) {
        (void)checked_snprintf(
            path, sizeof(path), "%s/ledger/resources/attachment--r%02u",
            layout.root, index);
        require(mkdir(path, 0700) == 0, "over-limit resource directory made");
    }
    expect_result(&layout, DCENT_RECEIPT_STORE_LIMIT,
                  "thirty-third resource is rejected before parsing");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, "ledger/resources");
    require(rmdir(path) == 0, "resources directory removed");
    require(symlink(".", path) == 0, "resources symlink substituted");
    expect_result(&layout, DCENT_RECEIPT_STORE_UNSAFE_METADATA,
                  "directory symlink substitution is rejected");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    require(chmod(layout.root, 0755) == 0, "unsafe lock mode installed");
    expect_result(&layout, DCENT_RECEIPT_STORE_UNSAFE_METADATA,
                  "lock directory mode is exact");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, "ledger/binding");
    require(chmod(path, 0644) == 0, "unsafe binding mode installed");
    expect_result(&layout, DCENT_RECEIPT_STORE_UNSAFE_METADATA,
                  "receipt mode is exact");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, "ledger/binding");
    require(chown(path, 1, 1) == 0, "foreign binding ownership installed");
    expect_result(&layout, DCENT_RECEIPT_STORE_UNSAFE_METADATA,
                  "receipt ownership is exact root:root");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, "ledger/binding");
    require(unlink(path) == 0, "binding removed for symlink fixture");
    require(symlink("../owner", path) == 0, "binding symlink installed");
    expect_result(&layout, DCENT_RECEIPT_STORE_UNSAFE_METADATA,
                  "receipt symlink is rejected before open");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, "ledger/binding");
    (void)checked_snprintf(other, sizeof(other), "%s.hard", layout.root);
    require(link(path, other) == 0, "external hard link created");
    expect_result(&layout, DCENT_RECEIPT_STORE_UNSAFE_METADATA,
                  "hard-linked receipt is rejected");
    require(unlink(other) == 0, "external hard link removed");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, "ledger/binding");
    require(unlink(path) == 0, "binding removed for FIFO fixture");
    require(mkfifo(path, 0600) == 0, "FIFO fixture created");
    expect_result(&layout, DCENT_RECEIPT_STORE_UNSAFE_METADATA,
                  "FIFO receipt is rejected without blocking");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, "ledger/binding");
    require(unlink(path) == 0, "binding removed for socket fixture");
    fd = create_unix_socket(path);
    expect_result(&layout, DCENT_RECEIPT_STORE_UNSAFE_METADATA,
                  "UNIX socket receipt is rejected before open");
    require(close(fd) == 0, "fixture UNIX socket descriptor closes");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, "ledger/binding");
    fd = open(path, O_WRONLY | O_TRUNC | O_CLOEXEC);
    require(fd >= 0 && close(fd) == 0, "empty binding installed");
    expect_result(&layout, DCENT_RECEIPT_STORE_UNSAFE_METADATA,
                  "zero-byte receipt is rejected before parsing");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, "ledger/binding");
    {
        char oversized[DCENT_RECEIPT_MAX_FILE + 1U];

        memset(oversized, 'x', sizeof(oversized));
        write_file(path, oversized, sizeof(oversized));
    }
    expect_result(&layout, DCENT_RECEIPT_STORE_UNSAFE_METADATA,
                  "4097-byte receipt is rejected before reading");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root, "owner");
    write_file(path, "malformed\n", strlen("malformed\n"));
    expect_result(&layout, DCENT_RECEIPT_STORE_FORMAT,
                  "malformed lock owner blocks binding authority");
    destroy_layout(&layout);

    create_layout(&layout, "ubi-1", false);
    path_join(path, sizeof(path), layout.root,
              "ledger/resources/attachment--ubi-1/status.5");
    write_file(path, "x", 1U);
    expect_result(&layout, DCENT_RECEIPT_STORE_LAYOUT,
                  "status revision five is a foreign entry");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root,
              "ledger/resources/future--resource");
    require(mkdir(path, 0700) == 0,
            "empty foreign resource directory created");
    expect_result(&layout, DCENT_RECEIPT_STORE_LAYOUT,
                  "empty foreign resource directory blocks inspection");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    path_join(path, sizeof(path), layout.root,
              "ledger/resources/attachment--not-a-directory");
    write_file(path, "x", 1U);
    expect_result(&layout, DCENT_RECEIPT_STORE_UNSAFE_METADATA,
                  "regular foreign resource entry violates exact topology");
    destroy_layout(&layout);
}

static void test_races_and_descriptor_lifecycle(void)
{
    struct test_layout layout;
    struct dcent_receipt_ledger_snapshot snapshot;
    struct dcent_receipt_store_error error;
    size_t before;
    size_t after;
    unsigned int index;

    create_layout(&layout, NULL, false);
    before = open_fd_count();
    for (index = 0U; index < 20U; ++index)
        require(scan_layout(&layout, &snapshot, &error) ==
                    DCENT_RECEIPT_STORE_OK,
                "repeated descriptor scan succeeds");
    after = open_fd_count();
    require(before == after, "successful scans leak no descriptors");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    before = open_fd_count();
    hook_mode = 1;
    hook_fired = false;
    expect_result(&layout, DCENT_RECEIPT_STORE_RACE,
                  "replacement after fstatat is detected by O_NOFOLLOW");
    require(hook_fired, "before-open substitution hook executed");
    hook_mode = 0;
    after = open_fd_count();
    require(before == after, "substitution failure leaks no descriptors");
    destroy_layout(&layout);

    create_layout(&layout, NULL, false);
    hook_mode = 2;
    hook_fired = false;
    expect_result(&layout, DCENT_RECEIPT_STORE_BINDING_MISMATCH,
                  "immutable owner substitution breaks the binding digest");
    require(hook_fired, "between-pass mutation hook executed");
    hook_mode = 0;
    destroy_layout(&layout);

    for (hook_mode = 3; hook_mode <= 6; ++hook_mode) {
        create_layout(&layout, NULL, false);
        hook_fired = false;
        expect_result(&layout, DCENT_RECEIPT_STORE_RACE,
                      "non-symlink replacement after fstatat is detected");
        require(hook_fired, "non-symlink replacement hook executed");
        destroy_layout(&layout);
    }
    hook_mode = 0;

    create_layout(&layout, NULL, false);
    hook_mode = 7;
    hook_fired = false;
    expect_result(&layout, DCENT_RECEIPT_STORE_RACE,
                  "same-shape resources directory replacement is detected");
    require(hook_fired, "resources-directory replacement hook executed");
    hook_mode = 0;
    destroy_layout(&layout);

    {
        struct dcent_receipt_ledger_snapshot sentinel;

        memset(&sentinel, 0xa5, sizeof(sentinel));
        snapshot = sentinel;
        require(dcent_receipt_store_scan_forensic_abi1(-1, &snapshot,
                                                       &error) ==
                    DCENT_RECEIPT_STORE_INVALID_ARGUMENT,
                "invalid descriptor is classified without filesystem access");
        require(memcmp(&snapshot, &sentinel, sizeof(snapshot)) == 0,
                "invalid descriptor leaves output byte-for-byte untouched");
        snapshot = sentinel;
        require(dcent_receipt_store_scan_forensic_abi1(-1, &snapshot, NULL) ==
                    DCENT_RECEIPT_STORE_INVALID_ARGUMENT &&
                    memcmp(&snapshot, &sentinel, sizeof(snapshot)) == 0,
                "optional NULL diagnostics preserve failure atomicity");
    }
    require(dcent_receipt_store_scan_forensic_abi1(0, NULL, &error) ==
                    DCENT_RECEIPT_STORE_INVALID_ARGUMENT &&
                error.result == DCENT_RECEIPT_STORE_INVALID_ARGUMENT,
            "NULL output is rejected with owned diagnostics");
    require(dcent_receipt_store_result_name(
                DCENT_RECEIPT_STORE_BINDING_MISMATCH) != NULL &&
                dcent_receipt_store_result_name(
                    (enum dcent_receipt_store_result)99) == NULL,
            "store result names are total only over defined values");
}

int main(void)
{
    size_t descriptors_before;

    (void)umask(0077);
    descriptors_before = open_fd_count();
    test_canonical_layouts();
    test_binding_and_topology_refusals();
    test_races_and_descriptor_lifecycle();
    require(descriptors_before == open_fd_count(),
            "all success and refusal branches leave the process FD-neutral");
    printf("dcentos-receipt descriptor store tests: %u assertions\n",
           assertions);
    return 0;
}
