/* SPDX-License-Identifier: GPL-3.0-or-later */
#define _GNU_SOURCE
#include "receipt_store.h"

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#define STORE_RESOURCE_NAME_MAX (10U + 2U + DCENT_RECEIPT_MAX_ID)
#define STORE_MAX_NAME STORE_RESOURCE_NAME_MAX
#define STORE_RESOURCE_INVENTORY_CAPACITY (DCENT_RECEIPT_MAX_RESOURCES + 1U)
#define STORE_CHAIN_INVENTORY_CAPACITY (DCENT_RECEIPT_MAX_REVISIONS + 2U)

#if defined(__GNUC__)
#define STORE_NOINLINE __attribute__((noinline))
#else
#define STORE_NOINLINE
#endif

struct store_entry {
    char name[STORE_MAX_NAME + 1U];
    struct stat metadata;
};

struct store_inventory {
    struct stat directory;
    size_t count;
    size_t capacity;
    struct store_entry *entries;
};

struct scan_context {
    struct dcent_receipt_store_error *error;
    size_t aggregate_bytes;
    dev_t filesystem;
};

static enum dcent_receipt_store_result store_error(
    struct scan_context *context, enum dcent_receipt_store_result result,
    int system_errno, int format_result, const char *name)
{
    struct dcent_receipt_store_error *error;

    if (context == NULL || context->error == NULL)
        return result;
    error = context->error;
    memset(error, 0, sizeof(*error));
    error->result = result;
    error->system_errno = system_errno;
    error->format_result = format_result;
    if (name != NULL) {
        size_t length = strnlen(name, sizeof(error->context) - 1U);

        memcpy(error->context, name, length);
        error->context[length] = '\0';
    }
    return result;
}

static bool stat_stable(const struct stat *left, const struct stat *right)
{
    return left->st_dev == right->st_dev &&
           left->st_ino == right->st_ino &&
           left->st_mode == right->st_mode &&
           left->st_nlink == right->st_nlink &&
           left->st_uid == right->st_uid &&
           left->st_gid == right->st_gid &&
           left->st_size == right->st_size &&
           left->st_mtim.tv_sec == right->st_mtim.tv_sec &&
           left->st_mtim.tv_nsec == right->st_mtim.tv_nsec &&
           left->st_ctim.tv_sec == right->st_ctim.tv_sec &&
           left->st_ctim.tv_nsec == right->st_ctim.tv_nsec;
}

static bool directory_metadata_safe(const struct stat *metadata,
                                    dev_t filesystem)
{
    return S_ISDIR(metadata->st_mode) &&
           (metadata->st_mode & 07777U) == 0700U &&
           metadata->st_uid == 0U && metadata->st_gid == 0U &&
           metadata->st_dev == filesystem;
}

static bool receipt_metadata_safe(const struct stat *metadata,
                                  dev_t filesystem)
{
    return S_ISREG(metadata->st_mode) &&
           (metadata->st_mode & 07777U) == 0600U &&
           metadata->st_uid == 0U && metadata->st_gid == 0U &&
           metadata->st_nlink == 1U && metadata->st_dev == filesystem &&
           metadata->st_size >= 1 &&
           (uintmax_t)metadata->st_size <= DCENT_RECEIPT_MAX_FILE;
}

static int entry_compare(const void *left, const void *right)
{
    const struct store_entry *a = left;
    const struct store_entry *b = right;

    return strcmp(a->name, b->name);
}

static const struct store_entry *inventory_find(
    const struct store_inventory *inventory, const char *name)
{
    size_t index;

    for (index = 0U; index < inventory->count; ++index) {
        if (strcmp(inventory->entries[index].name, name) == 0)
            return &inventory->entries[index];
    }
    return NULL;
}

static void inventory_init(struct store_inventory *inventory,
                           struct store_entry *entries, size_t capacity)
{
    memset(inventory, 0, sizeof(*inventory));
    inventory->entries = entries;
    inventory->capacity = capacity;
}

static bool inventory_equal(const struct store_inventory *left,
                            const struct store_inventory *right)
{
    size_t index;

    if (!stat_stable(&left->directory, &right->directory) ||
        left->count != right->count)
        return false;
    for (index = 0U; index < left->count; ++index) {
        if (strcmp(left->entries[index].name,
                   right->entries[index].name) != 0 ||
            !stat_stable(&left->entries[index].metadata,
                         &right->entries[index].metadata))
            return false;
    }
    return true;
}

static enum dcent_receipt_store_result scan_inventory(
    int directory_fd, struct store_inventory *inventory,
    struct scan_context *context, const char *label)
{
    struct store_inventory scanned;
    struct stat anchor;
    struct stat after;
    struct dirent *entry;
    DIR *stream = NULL;
    int scan_fd = -1;
    int saved_errno;
    enum dcent_receipt_store_result result = DCENT_RECEIPT_STORE_OK;

    inventory_init(&scanned, inventory->entries, inventory->capacity);
    if (fstat(directory_fd, &anchor) != 0)
        return store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0, label);
    scan_fd = openat(directory_fd, ".",
                     O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (scan_fd < 0)
        return store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0, label);
    if (fstat(scan_fd, &scanned.directory) != 0) {
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0, label);
        goto close_fd;
    }
    if (!stat_stable(&anchor, &scanned.directory)) {
        result = store_error(context, DCENT_RECEIPT_STORE_RACE, 0, 0, label);
        goto close_fd;
    }
    stream = fdopendir(scan_fd);
    if (stream == NULL) {
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0, label);
        goto close_fd;
    }
    scan_fd = -1;
    errno = 0;
    while ((entry = readdir(stream)) != NULL) {
        size_t length;
        struct store_entry *destination;

        if (strcmp(entry->d_name, ".") == 0 ||
            strcmp(entry->d_name, "..") == 0) {
            errno = 0;
            continue;
        }
        length = strlen(entry->d_name);
        if (length == 0U || length > STORE_MAX_NAME) {
            result = store_error(context, DCENT_RECEIPT_STORE_LAYOUT, 0, 0,
                                 label);
            goto close_stream;
        }
        if (scanned.count >= scanned.capacity) {
            result = store_error(context, DCENT_RECEIPT_STORE_LIMIT, 0, 0,
                                 label);
            goto close_stream;
        }
        destination = &scanned.entries[scanned.count];
        memcpy(destination->name, entry->d_name, length + 1U);
        if (fstatat(dirfd(stream), entry->d_name, &destination->metadata,
                    AT_SYMLINK_NOFOLLOW) != 0) {
            saved_errno = errno;
            result = store_error(
                context,
                saved_errno == ENOENT ? DCENT_RECEIPT_STORE_RACE
                                      : DCENT_RECEIPT_STORE_IO,
                saved_errno, 0, entry->d_name);
            goto close_stream;
        }
        ++scanned.count;
        errno = 0;
    }
    if (errno != 0) {
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0, label);
        goto close_stream;
    }
    if (fstat(dirfd(stream), &after) != 0) {
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0, label);
        goto close_stream;
    }
    if (!stat_stable(&scanned.directory, &after)) {
        result = store_error(context, DCENT_RECEIPT_STORE_RACE, 0, 0, label);
        goto close_stream;
    }
    qsort(scanned.entries, scanned.count, sizeof(scanned.entries[0]),
          entry_compare);
    *inventory = scanned;

close_stream:
    if (stream != NULL && closedir(stream) != 0 &&
        result == DCENT_RECEIPT_STORE_OK)
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0, label);
    return result;

close_fd:
    if (scan_fd >= 0 && close(scan_fd) != 0 &&
        result == DCENT_RECEIPT_STORE_OK)
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0, label);
    return result;
}

static enum dcent_receipt_store_result open_directory(
    int parent_fd, const char *name, const struct stat *expected,
    nlink_t expected_links, int *out_fd, struct scan_context *context)
{
    struct stat opened;
    int fd;

    if (!directory_metadata_safe(expected, context->filesystem) ||
        (expected_links != 0U && expected->st_nlink != expected_links))
        return store_error(context, DCENT_RECEIPT_STORE_UNSAFE_METADATA, 0, 0,
                           name);
#ifdef DCENT_RECEIPT_STORE_TESTING
    dcent_receipt_store_test_hook(DCENT_RECEIPT_STORE_TEST_BEFORE_OPEN,
                                  parent_fd, name);
#endif
    fd = openat(parent_fd, name,
                O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) {
        int saved_errno = errno;
        return store_error(
            context,
            saved_errno == ENOENT || saved_errno == ELOOP ||
                    saved_errno == ENOTDIR
                ? DCENT_RECEIPT_STORE_RACE
                : DCENT_RECEIPT_STORE_IO,
            saved_errno, 0, name);
    }
    if (fstat(fd, &opened) != 0) {
        int saved_errno = errno;
        (void)close(fd);
        return store_error(context, DCENT_RECEIPT_STORE_IO, saved_errno, 0,
                           name);
    }
    if (!directory_metadata_safe(&opened, context->filesystem) ||
        (expected_links != 0U && opened.st_nlink != expected_links) ||
        !stat_stable(expected, &opened)) {
        (void)close(fd);
        return store_error(context, DCENT_RECEIPT_STORE_RACE, 0, 0, name);
    }
    *out_fd = fd;
    return DCENT_RECEIPT_STORE_OK;
}

static enum dcent_receipt_store_result read_receipt(
    int parent_fd, const struct store_entry *entry, unsigned char *buffer,
    size_t *size, bool count_aggregate, struct scan_context *context)
{
    struct stat opened;
    struct stat after;
    struct stat named_after;
    off_t offset = 0;
    int fd;
    enum dcent_receipt_store_result result = DCENT_RECEIPT_STORE_OK;

    if (!receipt_metadata_safe(&entry->metadata, context->filesystem))
        return store_error(context, DCENT_RECEIPT_STORE_UNSAFE_METADATA, 0, 0,
                           entry->name);
    if (count_aggregate) {
        size_t bytes = (size_t)entry->metadata.st_size;

        if (bytes > DCENT_RECEIPT_MAX_LEDGER - context->aggregate_bytes)
            return store_error(context, DCENT_RECEIPT_STORE_LIMIT, 0, 0,
                               entry->name);
        context->aggregate_bytes += bytes;
    }
#ifdef DCENT_RECEIPT_STORE_TESTING
    dcent_receipt_store_test_hook(DCENT_RECEIPT_STORE_TEST_BEFORE_OPEN,
                                  parent_fd, entry->name);
#endif
    fd = openat(parent_fd, entry->name,
                O_RDONLY | O_NONBLOCK | O_NOCTTY | O_NOFOLLOW | O_CLOEXEC);
    if (fd < 0) {
        int saved_errno = errno;

        if (fstatat(parent_fd, entry->name, &opened,
                    AT_SYMLINK_NOFOLLOW) != 0) {
            int admission_errno = errno;

            return store_error(
                context,
                admission_errno == ENOENT ? DCENT_RECEIPT_STORE_RACE
                                           : DCENT_RECEIPT_STORE_IO,
                saved_errno, 0, entry->name);
        }
        return store_error(
            context,
            stat_stable(&entry->metadata, &opened)
                ? DCENT_RECEIPT_STORE_IO
                : DCENT_RECEIPT_STORE_RACE,
            saved_errno, 0, entry->name);
    }
    if (fstat(fd, &opened) != 0) {
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0,
                             entry->name);
        goto close_fd;
    }
    if (!receipt_metadata_safe(&opened, context->filesystem) ||
        !stat_stable(&entry->metadata, &opened)) {
        result = store_error(context, DCENT_RECEIPT_STORE_RACE, 0, 0,
                             entry->name);
        goto close_fd;
    }
    while (offset < opened.st_size) {
        ssize_t received = pread(fd, buffer + (size_t)offset,
                                 (size_t)(opened.st_size - offset), offset);

        if (received < 0 && errno == EINTR)
            continue;
        if (received <= 0) {
            result = store_error(
                context,
                received == 0 ? DCENT_RECEIPT_STORE_RACE
                              : DCENT_RECEIPT_STORE_IO,
                received < 0 ? errno : 0, 0, entry->name);
            goto close_fd;
        }
        offset += received;
    }
    for (;;) {
        unsigned char extra;
        ssize_t received = pread(fd, &extra, 1U, opened.st_size);

        if (received < 0 && errno == EINTR)
            continue;
        if (received != 0) {
            result = store_error(
                context,
                received < 0 ? DCENT_RECEIPT_STORE_IO
                             : DCENT_RECEIPT_STORE_RACE,
                received < 0 ? errno : 0, 0, entry->name);
            goto close_fd;
        }
        break;
    }
    if (fstat(fd, &after) != 0) {
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0,
                             entry->name);
        goto close_fd;
    }
    if (fstatat(parent_fd, entry->name, &named_after,
                AT_SYMLINK_NOFOLLOW) != 0) {
        int saved_errno = errno;
        result = store_error(
            context,
            saved_errno == ENOENT ? DCENT_RECEIPT_STORE_RACE
                                  : DCENT_RECEIPT_STORE_IO,
            saved_errno, 0, entry->name);
        goto close_fd;
    }
    if (!stat_stable(&opened, &after) ||
        !stat_stable(&opened, &named_after)) {
        result = store_error(context, DCENT_RECEIPT_STORE_RACE, 0, 0,
                             entry->name);
        goto close_fd;
    }
    *size = (size_t)opened.st_size;

close_fd:
    if (close(fd) != 0 && result == DCENT_RECEIPT_STORE_OK)
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0,
                             entry->name);
    return result;
}

static enum dcent_receipt_store_result parse_error(
    struct scan_context *context, int format_result, const char *name)
{
    return store_error(context, DCENT_RECEIPT_STORE_FORMAT, 0, format_result,
                       name);
}

static bool id_equal(const struct dcent_receipt_id *left,
                     const struct dcent_receipt_id *right)
{
    return left->size == right->size && left->size <= DCENT_RECEIPT_MAX_ID &&
           memcmp(left->bytes, right->bytes, left->size) == 0;
}

static bool binding_matches_authority(
    const struct dcent_receipt_binding_anchor *binding,
    const struct dcent_receipt_lock_anchor *lock, const struct stat *lock_stat,
    const struct stat *owner_stat, const struct stat *ledger_stat)
{
    return binding->initialized && lock->initialized &&
           id_equal(&binding->transaction_id, &lock->transaction_id) &&
           id_equal(&binding->boot_id, &lock->boot_id) &&
           binding->owner_pid == lock->pid &&
           binding->owner_starttime == lock->starttime &&
           binding->transaction_lock_device_inode.device ==
               (uint64_t)lock_stat->st_dev &&
           binding->transaction_lock_device_inode.inode ==
               (uint64_t)lock_stat->st_ino &&
           binding->transaction_lock_owner_device_inode.device ==
               (uint64_t)owner_stat->st_dev &&
           binding->transaction_lock_owner_device_inode.inode ==
               (uint64_t)owner_stat->st_ino &&
           binding->transaction_lock_owner_sha256.present &&
           memcmp(binding->transaction_lock_owner_sha256.bytes,
                  lock->record_sha256, DCENT_RECEIPT_SHA256_BYTES) == 0 &&
           binding->ledger_device_inode.device ==
               (uint64_t)ledger_stat->st_dev &&
           binding->ledger_device_inode.inode ==
               (uint64_t)ledger_stat->st_ino;
}

static enum dcent_receipt_store_result validate_chain_inventory(
    const struct store_inventory *inventory, struct scan_context *context,
    const char *label)
{
    bool intent = false;
    unsigned int status_mask = 0U;
    size_t index;
    unsigned int revisions;

    for (index = 0U; index < inventory->count; ++index) {
        const char *name = inventory->entries[index].name;

        if (strcmp(name, "intent") == 0) {
            if (intent)
                return store_error(context, DCENT_RECEIPT_STORE_LAYOUT, 0, 0,
                                   label);
            intent = true;
        } else if (strncmp(name, "status.", 7U) == 0 &&
                   name[7] >= '1' && name[7] <= '4' && name[8] == '\0') {
            unsigned int bit = 1U << (unsigned int)(name[7] - '1');

            if ((status_mask & bit) != 0U)
                return store_error(context, DCENT_RECEIPT_STORE_LAYOUT, 0, 0,
                                   label);
            status_mask |= bit;
        } else {
            return store_error(context, DCENT_RECEIPT_STORE_LAYOUT, 0, 0,
                               name);
        }
    }
    if (!intent || status_mask == 0U)
        return store_error(context, DCENT_RECEIPT_STORE_LAYOUT, 0, 0, label);
    revisions = 0U;
    while ((status_mask & (1U << revisions)) != 0U)
        ++revisions;
    if (revisions > DCENT_RECEIPT_MAX_REVISIONS ||
        status_mask != (1U << revisions) - 1U)
        return store_error(context, DCENT_RECEIPT_STORE_LAYOUT, 0, 0, label);
    return DCENT_RECEIPT_STORE_OK;
}

static STORE_NOINLINE enum dcent_receipt_store_result scan_resource(
    int resources_fd, const struct store_entry *directory_entry,
    const struct dcent_receipt_binding_anchor *binding,
    struct dcent_receipt_resource_chain *chain, struct scan_context *context)
{
    struct store_inventory before;
    struct store_inventory after;
    struct store_entry before_entries[STORE_CHAIN_INVENTORY_CAPACITY];
    struct store_entry after_entries[STORE_CHAIN_INVENTORY_CAPACITY];
    const struct store_entry *entry;
    struct dcent_receipt_resource_intent intent;
    struct dcent_receipt_resource_status status;
    unsigned char buffer[DCENT_RECEIPT_MAX_FILE];
    char expected_name[STORE_RESOURCE_NAME_MAX + 1U];
    size_t size;
    unsigned int revision;
    int resource_fd = -1;
    int format_result;
    int length;
    enum dcent_receipt_store_result result;

    inventory_init(&before, before_entries,
                   STORE_CHAIN_INVENTORY_CAPACITY);
    inventory_init(&after, after_entries, STORE_CHAIN_INVENTORY_CAPACITY);
    result = open_directory(resources_fd, directory_entry->name,
                            &directory_entry->metadata, 2U, &resource_fd,
                            context);
    if (result != DCENT_RECEIPT_STORE_OK)
        return result;
    result = scan_inventory(resource_fd, &before, context,
                            directory_entry->name);
    if (result != DCENT_RECEIPT_STORE_OK)
        goto close_resource;
    result = validate_chain_inventory(&before, context, directory_entry->name);
    if (result != DCENT_RECEIPT_STORE_OK)
        goto close_resource;
    entry = inventory_find(&before, "intent");
    result = read_receipt(resource_fd, entry, buffer, &size, true, context);
    if (result != DCENT_RECEIPT_STORE_OK)
        goto close_resource;
    format_result = dcent_receipt_parse_resource_intent_abi1(buffer, size,
                                                              &intent);
    if (format_result != DCENT_RECEIPT_FORMAT_OK) {
        result = parse_error(context, format_result, "intent");
        goto close_resource;
    }
    length = snprintf(expected_name, sizeof(expected_name), "%s--%.*s",
                      dcent_receipt_resource_kind_name(intent.kind),
                      (int)intent.resource_id.size, intent.resource_id.data);
    if (length < 0 || (size_t)length >= sizeof(expected_name) ||
        strcmp(expected_name, directory_entry->name) != 0) {
        result = store_error(context, DCENT_RECEIPT_STORE_BINDING_MISMATCH, 0,
                             0, directory_entry->name);
        goto close_resource;
    }
    format_result = dcent_receipt_resource_chain_begin(chain, binding, &intent);
    if (format_result != DCENT_RECEIPT_FORMAT_OK) {
        result = parse_error(context, format_result, "intent");
        goto close_resource;
    }
    for (revision = 1U; revision <= DCENT_RECEIPT_MAX_REVISIONS; ++revision) {
        char status_name[9];

        (void)snprintf(status_name, sizeof(status_name), "status.%u", revision);
        entry = inventory_find(&before, status_name);
        if (entry == NULL)
            break;
        result = read_receipt(resource_fd, entry, buffer, &size, true, context);
        if (result != DCENT_RECEIPT_STORE_OK)
            goto close_resource;
        format_result = dcent_receipt_parse_resource_status_abi1(buffer, size,
                                                                  &status);
        if (format_result == DCENT_RECEIPT_FORMAT_OK)
            format_result = dcent_receipt_resource_chain_add(chain, &status);
        if (format_result != DCENT_RECEIPT_FORMAT_OK) {
            result = parse_error(context, format_result, status_name);
            goto close_resource;
        }
    }
    format_result = dcent_receipt_resource_chain_finish(chain);
    if (format_result != DCENT_RECEIPT_FORMAT_OK) {
        result = parse_error(context, format_result, directory_entry->name);
        goto close_resource;
    }
    result = scan_inventory(resource_fd, &after, context,
                            directory_entry->name);
    if (result == DCENT_RECEIPT_STORE_OK &&
        !inventory_equal(&before, &after))
        result = store_error(context, DCENT_RECEIPT_STORE_RACE, 0, 0,
                             directory_entry->name);

close_resource:
    if (close(resource_fd) != 0 && result == DCENT_RECEIPT_STORE_OK)
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0,
                             directory_entry->name);
    return result;
}

static STORE_NOINLINE enum dcent_receipt_store_result scan_claim(
    int ledger_fd, const struct store_entry *directory_entry,
    const struct dcent_receipt_binding_anchor *binding,
    struct dcent_receipt_claim_chain *chain, struct scan_context *context)
{
    struct store_inventory before;
    struct store_inventory after;
    struct store_entry before_entries[STORE_CHAIN_INVENTORY_CAPACITY];
    struct store_entry after_entries[STORE_CHAIN_INVENTORY_CAPACITY];
    const struct store_entry *entry;
    struct dcent_receipt_claim_intent intent;
    struct dcent_receipt_claim_status status;
    unsigned char buffer[DCENT_RECEIPT_MAX_FILE];
    size_t size;
    unsigned int revision;
    int claim_fd = -1;
    int format_result;
    enum dcent_receipt_store_result result;

    inventory_init(&before, before_entries,
                   STORE_CHAIN_INVENTORY_CAPACITY);
    inventory_init(&after, after_entries, STORE_CHAIN_INVENTORY_CAPACITY);
    result = open_directory(ledger_fd, directory_entry->name,
                            &directory_entry->metadata, 2U, &claim_fd, context);
    if (result != DCENT_RECEIPT_STORE_OK)
        return result;
    result = scan_inventory(claim_fd, &before, context, "reconcile.claim");
    if (result != DCENT_RECEIPT_STORE_OK)
        goto close_claim;
    result = validate_chain_inventory(&before, context, "reconcile.claim");
    if (result != DCENT_RECEIPT_STORE_OK)
        goto close_claim;
    entry = inventory_find(&before, "intent");
    result = read_receipt(claim_fd, entry, buffer, &size, true, context);
    if (result != DCENT_RECEIPT_STORE_OK)
        goto close_claim;
    format_result = dcent_receipt_parse_claim_intent_abi1(buffer, size,
                                                           &intent);
    if (format_result == DCENT_RECEIPT_FORMAT_OK)
        format_result = dcent_receipt_claim_chain_begin(chain, binding, &intent);
    if (format_result != DCENT_RECEIPT_FORMAT_OK) {
        result = parse_error(context, format_result, "reconcile.claim/intent");
        goto close_claim;
    }
    for (revision = 1U; revision <= DCENT_RECEIPT_MAX_REVISIONS; ++revision) {
        char status_name[9];

        (void)snprintf(status_name, sizeof(status_name), "status.%u", revision);
        entry = inventory_find(&before, status_name);
        if (entry == NULL)
            break;
        result = read_receipt(claim_fd, entry, buffer, &size, true, context);
        if (result != DCENT_RECEIPT_STORE_OK)
            goto close_claim;
        format_result = dcent_receipt_parse_claim_status_abi1(buffer, size,
                                                               &status);
        if (format_result == DCENT_RECEIPT_FORMAT_OK)
            format_result = dcent_receipt_claim_chain_add(chain, &status);
        if (format_result != DCENT_RECEIPT_FORMAT_OK) {
            result = parse_error(context, format_result, status_name);
            goto close_claim;
        }
    }
    format_result = dcent_receipt_claim_chain_finish(chain);
    if (format_result != DCENT_RECEIPT_FORMAT_OK) {
        result = parse_error(context, format_result, "reconcile.claim");
        goto close_claim;
    }
    result = scan_inventory(claim_fd, &after, context, "reconcile.claim");
    if (result == DCENT_RECEIPT_STORE_OK &&
        !inventory_equal(&before, &after))
        result = store_error(context, DCENT_RECEIPT_STORE_RACE, 0, 0,
                             "reconcile.claim");

close_claim:
    if (close(claim_fd) != 0 && result == DCENT_RECEIPT_STORE_OK)
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0,
                             "reconcile.claim");
    return result;
}

static enum dcent_receipt_store_result scan_once(
    int caller_lock_fd, struct dcent_receipt_ledger_snapshot *snapshot,
    struct scan_context *context)
{
    struct dcent_receipt_ledger_snapshot scanned;
    struct store_inventory lock_before;
    struct store_inventory ledger_before;
    struct store_inventory resources_before;
    struct store_inventory final_inventory;
    struct store_entry lock_entries[3U];
    struct store_entry ledger_entries[4U];
    struct store_entry
        resource_entries[STORE_RESOURCE_INVENTORY_CAPACITY];
    struct store_entry final_entries[STORE_RESOURCE_INVENTORY_CAPACITY];
    const struct store_entry *owner_entry;
    const struct store_entry *ledger_entry;
    const struct store_entry *binding_entry;
    const struct store_entry *resources_entry;
    const struct store_entry *claim_entry;
    struct dcent_receipt_lock_owner owner;
    struct dcent_receipt_binding binding;
    unsigned char buffer[DCENT_RECEIPT_MAX_FILE];
    struct stat caller_lock_stat;
    struct stat lock_stat;
    size_t size;
    size_t index;
    int lock_fd = -1;
    int ledger_fd = -1;
    int resources_fd = -1;
    int format_result;
    enum dcent_receipt_store_result result = DCENT_RECEIPT_STORE_OK;

    memset(&scanned, 0, sizeof(scanned));
    inventory_init(&lock_before, lock_entries,
                   sizeof(lock_entries) / sizeof(lock_entries[0]));
    inventory_init(&ledger_before, ledger_entries,
                   sizeof(ledger_entries) / sizeof(ledger_entries[0]));
    inventory_init(&resources_before, resource_entries,
                   STORE_RESOURCE_INVENTORY_CAPACITY);
    inventory_init(&final_inventory, final_entries,
                   STORE_RESOURCE_INVENTORY_CAPACITY);
    context->aggregate_bytes = 0U;
    if (fstat(caller_lock_fd, &caller_lock_stat) != 0)
        return store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0, "lock");
    context->filesystem = caller_lock_stat.st_dev;
    if (!directory_metadata_safe(&caller_lock_stat, context->filesystem) ||
        caller_lock_stat.st_nlink != 3U)
        return store_error(context, DCENT_RECEIPT_STORE_UNSAFE_METADATA, 0, 0,
                           "lock");
    lock_fd = openat(caller_lock_fd, ".",
                     O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    if (lock_fd < 0)
        return store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0, "lock");
    if (fstat(lock_fd, &lock_stat) != 0) {
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0, "lock");
        goto cleanup;
    }
    if (!stat_stable(&caller_lock_stat, &lock_stat)) {
        result = store_error(context, DCENT_RECEIPT_STORE_RACE, 0, 0, "lock");
        goto cleanup;
    }
    result = scan_inventory(lock_fd, &lock_before, context, "lock");
    if (result != DCENT_RECEIPT_STORE_OK)
        goto cleanup;
    if (lock_before.count != 2U ||
        (owner_entry = inventory_find(&lock_before, "owner")) == NULL ||
        (ledger_entry = inventory_find(&lock_before, "ledger")) == NULL) {
        result = store_error(context, DCENT_RECEIPT_STORE_LAYOUT, 0, 0, "lock");
        goto cleanup;
    }
    result = read_receipt(lock_fd, owner_entry, buffer, &size, false, context);
    if (result != DCENT_RECEIPT_STORE_OK)
        goto cleanup;
    format_result = dcent_receipt_parse_lock_owner_v3(buffer, size, &owner);
    if (format_result == DCENT_RECEIPT_FORMAT_OK)
        format_result = dcent_receipt_lock_anchor_init(&scanned.lock, &owner);
    if (format_result != DCENT_RECEIPT_FORMAT_OK) {
        result = parse_error(context, format_result, "owner");
        goto cleanup;
    }
    result = open_directory(lock_fd, "ledger", &ledger_entry->metadata, 0U,
                            &ledger_fd, context);
    if (result != DCENT_RECEIPT_STORE_OK)
        goto cleanup;
    result = scan_inventory(ledger_fd, &ledger_before, context, "ledger");
    if (result != DCENT_RECEIPT_STORE_OK)
        goto cleanup;
    if ((ledger_before.count != 2U && ledger_before.count != 3U) ||
        (binding_entry = inventory_find(&ledger_before, "binding")) == NULL ||
        (resources_entry = inventory_find(&ledger_before, "resources")) ==
            NULL) {
        result = store_error(context, DCENT_RECEIPT_STORE_LAYOUT, 0, 0,
                             "ledger");
        goto cleanup;
    }
    claim_entry = inventory_find(&ledger_before, "reconcile.claim");
    if ((claim_entry != NULL) != (ledger_before.count == 3U)) {
        result = store_error(context, DCENT_RECEIPT_STORE_LAYOUT, 0, 0,
                             "ledger");
        goto cleanup;
    }
    if (ledger_before.directory.st_nlink != (claim_entry == NULL ? 3U : 4U)) {
        result = store_error(context, DCENT_RECEIPT_STORE_UNSAFE_METADATA, 0, 0,
                             "ledger");
        goto cleanup;
    }
    result = read_receipt(ledger_fd, binding_entry, buffer, &size, true,
                          context);
    if (result != DCENT_RECEIPT_STORE_OK)
        goto cleanup;
    format_result = dcent_receipt_parse_binding_abi1(buffer, size, &binding);
    if (format_result == DCENT_RECEIPT_FORMAT_OK)
        format_result = dcent_receipt_binding_anchor_init(&scanned.binding,
                                                          &binding);
    if (format_result != DCENT_RECEIPT_FORMAT_OK) {
        result = parse_error(context, format_result, "binding");
        goto cleanup;
    }
    if (!binding_matches_authority(&scanned.binding, &scanned.lock, &lock_stat,
                                   &owner_entry->metadata,
                                   &ledger_before.directory)) {
        result = store_error(context, DCENT_RECEIPT_STORE_BINDING_MISMATCH, 0,
                             0, "binding");
        goto cleanup;
    }
    result = open_directory(ledger_fd, "resources",
                            &resources_entry->metadata, 0U, &resources_fd,
                            context);
    if (result != DCENT_RECEIPT_STORE_OK)
        goto cleanup;
    result = scan_inventory(resources_fd, &resources_before, context,
                            "resources");
    if (result != DCENT_RECEIPT_STORE_OK)
        goto cleanup;
    if (resources_before.count > DCENT_RECEIPT_MAX_RESOURCES) {
        result = store_error(context, DCENT_RECEIPT_STORE_LIMIT, 0, 0,
                             "resources");
        goto cleanup;
    }
    if (resources_before.directory.st_nlink !=
        (nlink_t)(2U + resources_before.count)) {
        result = store_error(context, DCENT_RECEIPT_STORE_UNSAFE_METADATA, 0, 0,
                             "resources");
        goto cleanup;
    }
    for (index = 0U; index < resources_before.count; ++index) {
        result = scan_resource(resources_fd, &resources_before.entries[index],
                               &scanned.binding,
                               &scanned.resources[scanned.resource_count],
                               context);
        if (result != DCENT_RECEIPT_STORE_OK)
            goto cleanup;
        ++scanned.resource_count;
    }
    if (claim_entry != NULL) {
        result = scan_claim(ledger_fd, claim_entry, &scanned.binding,
                            &scanned.claim, context);
        if (result != DCENT_RECEIPT_STORE_OK)
            goto cleanup;
        scanned.claim_present = true;
    }
    format_result = dcent_receipt_ledger_validate_summary(
        &scanned.binding, scanned.resources, scanned.resource_count,
        scanned.claim_present ? &scanned.claim : NULL,
        context->aggregate_bytes);
    if (format_result != DCENT_RECEIPT_FORMAT_OK) {
        result = parse_error(context, format_result, "ledger-summary");
        goto cleanup;
    }
    scanned.aggregate_bytes = context->aggregate_bytes;
    result = scan_inventory(resources_fd, &final_inventory, context,
                            "resources");
    if (result != DCENT_RECEIPT_STORE_OK)
        goto cleanup;
    if (!inventory_equal(&resources_before, &final_inventory)) {
        result = store_error(context, DCENT_RECEIPT_STORE_RACE, 0, 0,
                             "resources-readmission");
        goto cleanup;
    }
    result = scan_inventory(ledger_fd, &final_inventory, context, "ledger");
    if (result != DCENT_RECEIPT_STORE_OK)
        goto cleanup;
    if (!inventory_equal(&ledger_before, &final_inventory)) {
        result = store_error(context, DCENT_RECEIPT_STORE_RACE, 0, 0,
                             "ledger-readmission");
        goto cleanup;
    }
    result = scan_inventory(lock_fd, &final_inventory, context, "lock");
    if (result != DCENT_RECEIPT_STORE_OK)
        goto cleanup;
    if (!inventory_equal(&lock_before, &final_inventory)) {
        result = store_error(context, DCENT_RECEIPT_STORE_RACE, 0, 0,
                             "lock-readmission");
        goto cleanup;
    }
    *snapshot = scanned;

cleanup:
    if (resources_fd >= 0 && close(resources_fd) != 0 &&
        result == DCENT_RECEIPT_STORE_OK)
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0,
                             "resources");
    if (ledger_fd >= 0 && close(ledger_fd) != 0 &&
        result == DCENT_RECEIPT_STORE_OK)
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0,
                             "ledger");
    if (lock_fd >= 0 && close(lock_fd) != 0 &&
        result == DCENT_RECEIPT_STORE_OK)
        result = store_error(context, DCENT_RECEIPT_STORE_IO, errno, 0,
                             "lock");
    return result;
}

enum dcent_receipt_store_result dcent_receipt_store_scan_forensic_abi1(
    int lock_directory_fd, struct dcent_receipt_ledger_snapshot *out,
    struct dcent_receipt_store_error *error)
{
    struct dcent_receipt_ledger_snapshot scanned;
    unsigned char first_digest[DCENT_RECEIPT_SHA256_BYTES];
    unsigned char second_digest[DCENT_RECEIPT_SHA256_BYTES];
    struct scan_context context;
    enum dcent_receipt_store_result result;

    memset(&context, 0, sizeof(context));
    context.error = error;
    if (error != NULL)
        memset(error, 0, sizeof(*error));
    if (lock_directory_fd < 0 || out == NULL)
        return store_error(&context, DCENT_RECEIPT_STORE_INVALID_ARGUMENT,
                           EINVAL, 0, "scan");
    memset(&scanned, 0, sizeof(scanned));
    result = scan_once(lock_directory_fd, &scanned, &context);
    if (result != DCENT_RECEIPT_STORE_OK)
        return result;
    dcent_receipt_sha256(first_digest, &scanned, sizeof(scanned));
#ifdef DCENT_RECEIPT_STORE_TESTING
    dcent_receipt_store_test_hook(DCENT_RECEIPT_STORE_TEST_BETWEEN_PASSES,
                                  lock_directory_fd, NULL);
#endif
    memset(&scanned, 0, sizeof(scanned));
    result = scan_once(lock_directory_fd, &scanned, &context);
    if (result != DCENT_RECEIPT_STORE_OK)
        return result;
    dcent_receipt_sha256(second_digest, &scanned, sizeof(scanned));
    if (memcmp(first_digest, second_digest, sizeof(first_digest)) != 0)
        return store_error(&context, DCENT_RECEIPT_STORE_RACE, 0, 0,
                           "double-scan");
    *out = scanned;
    if (error != NULL)
        memset(error, 0, sizeof(*error));
    return DCENT_RECEIPT_STORE_OK;
}

const char *dcent_receipt_store_result_name(
    enum dcent_receipt_store_result result)
{
    switch (result) {
    case DCENT_RECEIPT_STORE_OK:
        return "ok";
    case DCENT_RECEIPT_STORE_INVALID_ARGUMENT:
        return "invalid-argument";
    case DCENT_RECEIPT_STORE_IO:
        return "io";
    case DCENT_RECEIPT_STORE_UNSAFE_METADATA:
        return "unsafe-metadata";
    case DCENT_RECEIPT_STORE_LAYOUT:
        return "layout";
    case DCENT_RECEIPT_STORE_FORMAT:
        return "format";
    case DCENT_RECEIPT_STORE_BINDING_MISMATCH:
        return "binding-mismatch";
    case DCENT_RECEIPT_STORE_LIMIT:
        return "limit";
    case DCENT_RECEIPT_STORE_RACE:
        return "race";
    }
    return NULL;
}
