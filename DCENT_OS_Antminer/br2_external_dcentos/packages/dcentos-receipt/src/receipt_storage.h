/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef DCENTOS_RECEIPT_STORAGE_H
#define DCENTOS_RECEIPT_STORAGE_H

#include "receipt_format.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define DCENT_RECEIPT_STORAGE_MAX_SEAL 4096U
#define DCENT_RECEIPT_STORAGE_MAX_HEAD 8192U
#define DCENT_RECEIPT_STORAGE_MAX_PHASE_REVISIONS \
    DCENT_RECEIPT_MAX_PHASE_REVISIONS
#define DCENT_RECEIPT_STORAGE_MAX_GENERATION \
    DCENT_RECEIPT_MAX_LEDGER_GENERATION

/*
 * Pure storage-ABI2 byte and state validation. These functions allocate
 * nothing, perform no filesystem access, and leave every output object
 * byte-for-byte untouched on failure. Parsed objects own all retained data.
 *
 * Delta validation classifies only the manifest mutation. Referenced ABI1
 * intent/status bytes still have to prove pending/claimed phases, legal phase
 * transitions, actor identity, and reconciliation preconditions before a
 * manifest may be used as mutation authority.
 */
enum dcent_receipt_storage_result {
    DCENT_RECEIPT_STORAGE_OK = 0,
    DCENT_RECEIPT_STORAGE_INVALID_ARGUMENT,
    DCENT_RECEIPT_STORAGE_MALFORMED,
    DCENT_RECEIPT_STORAGE_LIMIT,
    DCENT_RECEIPT_STORAGE_SEMANTIC,
};

struct dcent_receipt_storage_seal {
    bool initialized;
    struct dcent_receipt_id transaction_id;
    struct dcent_receipt_id boot_id;
    struct dcent_receipt_devino acquisition_guard_device_inode;
    struct dcent_receipt_devino transaction_lock_device_inode;
    struct dcent_receipt_devino transaction_lock_owner_device_inode;
    unsigned char
        transaction_lock_owner_sha256[DCENT_RECEIPT_SHA256_BYTES];
    uint64_t storage_mount_id;
    struct dcent_receipt_devino ledger_device_inode;
    unsigned char binding_sha256[DCENT_RECEIPT_SHA256_BYTES];
    struct dcent_receipt_devino mutation_lease_device_inode;
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_storage_resource_head {
    enum dcent_receipt_resource_kind kind;
    struct dcent_receipt_id resource_id;
    unsigned char intent_sha256[DCENT_RECEIPT_SHA256_BYTES];
    uint32_t status_revision;
    unsigned char status_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_storage_head {
    bool initialized;
    unsigned char seal_sha256[DCENT_RECEIPT_SHA256_BYTES];
    uint32_t generation;
    bool previous_present;
    uint32_t previous_generation;
    unsigned char previous_head_sha256[DCENT_RECEIPT_SHA256_BYTES];
    enum dcent_receipt_chain_authority authority;
    struct dcent_receipt_id authority_id;
    enum dcent_receipt_lock_phase transaction_phase;
    uint32_t transaction_phase_revision;
    bool transaction_phase_status_present;
    unsigned char
        transaction_phase_status_sha256[DCENT_RECEIPT_SHA256_BYTES];
    bool claim_present;
    struct dcent_receipt_id claim_id;
    unsigned char claim_intent_sha256[DCENT_RECEIPT_SHA256_BYTES];
    uint32_t claim_status_revision;
    unsigned char claim_status_sha256[DCENT_RECEIPT_SHA256_BYTES];
    size_t resource_count;
    struct dcent_receipt_storage_resource_head
        resources[DCENT_RECEIPT_MAX_RESOURCES];
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

enum dcent_receipt_storage_delta_kind {
    DCENT_RECEIPT_STORAGE_DELTA_INVALID = 0,
    DCENT_RECEIPT_STORAGE_DELTA_RESOURCE_ADD,
    DCENT_RECEIPT_STORAGE_DELTA_RESOURCE_ADVANCE,
    DCENT_RECEIPT_STORAGE_DELTA_CLAIM_ADD,
    DCENT_RECEIPT_STORAGE_DELTA_CLAIM_ADVANCE,
    DCENT_RECEIPT_STORAGE_DELTA_PHASE_ADVANCE,
};

struct dcent_receipt_storage_delta {
    bool initialized;
    enum dcent_receipt_storage_delta_kind kind;
    enum dcent_receipt_resource_kind resource_kind;
    struct dcent_receipt_id object_id;
    uint32_t previous_revision;
    uint32_t target_revision;
    enum dcent_receipt_lock_phase previous_phase;
    enum dcent_receipt_lock_phase target_phase;
};

struct dcent_receipt_storage_manifest_pair {
    bool initialized;
    bool genesis;
    unsigned int previous_bank;
    unsigned int current_bank;
    uint32_t current_generation;
    unsigned char current_head_sha256[DCENT_RECEIPT_SHA256_BYTES];
    struct dcent_receipt_storage_delta delta;
};

enum dcent_receipt_storage_result dcent_receipt_storage_parse_seal_abi2(
    const void *data, size_t size, struct dcent_receipt_storage_seal *out);

enum dcent_receipt_storage_result dcent_receipt_storage_parse_head_abi2(
    const void *data, size_t size, struct dcent_receipt_storage_head *out);

enum dcent_receipt_storage_result
dcent_receipt_storage_validate_manifest_pair_abi2(
    const struct dcent_receipt_storage_seal *seal,
    const struct dcent_receipt_storage_head *bank0,
    const struct dcent_receipt_storage_head *bank1,
    struct dcent_receipt_storage_manifest_pair *out);

const char *dcent_receipt_storage_result_name(
    enum dcent_receipt_storage_result result);

#endif
