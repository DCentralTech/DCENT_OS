/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef DCENTOS_RECEIPT_FORMAT_H
#define DCENTOS_RECEIPT_FORMAT_H

#include "receipt_state.h"
#include "sha256.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define DCENT_RECEIPT_MAX_RESOURCES 32U
#define DCENT_RECEIPT_MAX_REVISIONS 4U
#define DCENT_RECEIPT_MAX_PHASE_REVISIONS 16U
#define DCENT_RECEIPT_MAX_LEDGER_GENERATION                                \
    ((DCENT_RECEIPT_MAX_RESOURCES * DCENT_RECEIPT_MAX_REVISIONS) +         \
     DCENT_RECEIPT_MAX_REVISIONS + DCENT_RECEIPT_MAX_PHASE_REVISIONS)
#define DCENT_RECEIPT_MAX_EVIDENCE 1024U
#define DCENT_RECEIPT_MAX_EVIDENCE_LINES 32U
#define DCENT_RECEIPT_MAX_ID 64U
#define DCENT_RECEIPT_MAX_PATH 512U
#define DCENT_RECEIPT_MAX_EVIDENCE_TYPE 48U
#define DCENT_RECEIPT_MAX_FILE 4096U
#define DCENT_RECEIPT_MAX_HEADER_LINE 640U
#define DCENT_RECEIPT_MAX_LEDGER 393216U

/*
 * Pure ABI1 byte and chain validation.  The parsers allocate nothing and make
 * no filesystem calls.  Successful parser slices borrow the caller's input
 * buffer, which must remain alive while the parsed record is used.  Chain
 * summaries copy every identifier they retain, so record buffers may be reused
 * after a successful chain operation.  On any non-OK parser result the output
 * object is left byte-for-byte untouched.
 *
 * Chain operations accept only records and bindings returned successfully by
 * these parsers.  A caller-created struct is not an admitted ABI record.
 *
 * Registered evidence is validated only as a canonical authenticated envelope
 * here.  Kind-specific observation semantics belong to separate validators and
 * must exist before a caller may treat the body as hardware authority.
 */

enum dcent_receipt_format_result {
    DCENT_RECEIPT_FORMAT_OK = 0,
    DCENT_RECEIPT_FORMAT_MALFORMED,
    DCENT_RECEIPT_FORMAT_LIMIT,
    DCENT_RECEIPT_FORMAT_DIGEST_MISMATCH,
    DCENT_RECEIPT_FORMAT_SEMANTIC,
};

enum dcent_receipt_resource_kind {
    DCENT_RECEIPT_KIND_INVALID = 0,
    DCENT_RECEIPT_KIND_ATTACHMENT,
    DCENT_RECEIPT_KIND_NODE,
    DCENT_RECEIPT_KIND_MOUNT,
    DCENT_RECEIPT_KIND_WORKSPACE,
};

enum dcent_receipt_actor_kind {
    DCENT_RECEIPT_ACTOR_INVALID = 0,
    DCENT_RECEIPT_ACTOR_OWNER,
    DCENT_RECEIPT_ACTOR_RECONCILER,
};

enum dcent_receipt_chain_authority {
    DCENT_RECEIPT_AUTHORITY_INVALID = 0,
    DCENT_RECEIPT_AUTHORITY_OWNER,
    DCENT_RECEIPT_AUTHORITY_RECONCILER,
};

enum dcent_receipt_lock_phase {
    DCENT_RECEIPT_LOCK_INVALID = 0,
    DCENT_RECEIPT_LOCK_ACTIVE,
    DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED,
    DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED,
    DCENT_RECEIPT_LOCK_ENV_COMMITTED,
};

enum dcent_receipt_evidence_type {
    DCENT_RECEIPT_EVIDENCE_INVALID = 0,
    DCENT_RECEIPT_EVIDENCE_NONE,
    DCENT_RECEIPT_EVIDENCE_ATTACHMENT_INTENT,
    DCENT_RECEIPT_EVIDENCE_ATTACHMENT_ACTIVE,
    DCENT_RECEIPT_EVIDENCE_ATTACHMENT_RELEASE_PENDING,
    DCENT_RECEIPT_EVIDENCE_ATTACHMENT_RELEASED,
    DCENT_RECEIPT_EVIDENCE_ATTACHMENT_CONFLICT,
    DCENT_RECEIPT_EVIDENCE_NODE_INTENT,
    DCENT_RECEIPT_EVIDENCE_NODE_ACTIVE,
    DCENT_RECEIPT_EVIDENCE_NODE_RELEASE_PENDING,
    DCENT_RECEIPT_EVIDENCE_NODE_RELEASED,
    DCENT_RECEIPT_EVIDENCE_NODE_CONFLICT,
    DCENT_RECEIPT_EVIDENCE_MOUNT_INTENT,
    DCENT_RECEIPT_EVIDENCE_MOUNT_ACTIVE,
    DCENT_RECEIPT_EVIDENCE_MOUNT_RELEASE_PENDING,
    DCENT_RECEIPT_EVIDENCE_MOUNT_RELEASED,
    DCENT_RECEIPT_EVIDENCE_MOUNT_CONFLICT,
    DCENT_RECEIPT_EVIDENCE_WORKSPACE_INTENT,
    DCENT_RECEIPT_EVIDENCE_WORKSPACE_ACTIVE,
    DCENT_RECEIPT_EVIDENCE_WORKSPACE_RELEASE_PENDING,
    DCENT_RECEIPT_EVIDENCE_WORKSPACE_RELEASED,
    DCENT_RECEIPT_EVIDENCE_WORKSPACE_CONFLICT,
    DCENT_RECEIPT_EVIDENCE_OWNER_DEATH,
    DCENT_RECEIPT_EVIDENCE_MAINTENANCE_QUIESCENCE,
    DCENT_RECEIPT_EVIDENCE_RECONCILIATION_BEGIN,
    DCENT_RECEIPT_EVIDENCE_RECONCILIATION_COMPLETE,
    DCENT_RECEIPT_EVIDENCE_RECONCILIATION_BLOCKED,
    DCENT_RECEIPT_EVIDENCE_TRANSACTION_CLEANUP_REQUIRED,
    DCENT_RECEIPT_EVIDENCE_TRANSACTION_ENV_COMMIT_ARMED,
    DCENT_RECEIPT_EVIDENCE_TRANSACTION_ENV_COMMIT_DISARMED,
    DCENT_RECEIPT_EVIDENCE_TRANSACTION_ENV_COMMITTED,
};

struct dcent_receipt_slice {
    const unsigned char *data;
    size_t size;
};

struct dcent_receipt_id {
    size_t size;
    unsigned char bytes[DCENT_RECEIPT_MAX_ID];
};

struct dcent_receipt_path {
    size_t size;
    unsigned char bytes[DCENT_RECEIPT_MAX_PATH];
};

struct dcent_receipt_digest {
    bool present;
    unsigned char bytes[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_devino {
    uint64_t device;
    uint64_t inode;
};

struct dcent_receipt_evidence {
    enum dcent_receipt_evidence_type type;
    struct dcent_receipt_slice body;
    struct dcent_receipt_digest digest;
};

struct dcent_receipt_binding {
    struct dcent_receipt_slice transaction_id;
    struct dcent_receipt_slice boot_id;
    uint32_t owner_pid;
    uint64_t owner_starttime;
    struct dcent_receipt_devino owner_mount_namespace;
    struct dcent_receipt_devino acquisition_guard_device_inode;
    struct dcent_receipt_slice transaction_lock_path;
    struct dcent_receipt_devino transaction_lock_device_inode;
    struct dcent_receipt_devino transaction_lock_owner_device_inode;
    struct dcent_receipt_digest transaction_lock_owner_sha256;
    uint64_t storage_mount_id;
    struct dcent_receipt_slice ledger_path;
    struct dcent_receipt_devino ledger_device_inode;
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_binding_anchor {
    bool initialized;
    struct dcent_receipt_id transaction_id;
    struct dcent_receipt_id boot_id;
    uint32_t owner_pid;
    uint64_t owner_starttime;
    struct dcent_receipt_devino owner_mount_namespace;
    struct dcent_receipt_devino acquisition_guard_device_inode;
    struct dcent_receipt_path transaction_lock_path;
    struct dcent_receipt_devino transaction_lock_device_inode;
    struct dcent_receipt_devino transaction_lock_owner_device_inode;
    struct dcent_receipt_digest transaction_lock_owner_sha256;
    uint64_t storage_mount_id;
    struct dcent_receipt_path ledger_path;
    struct dcent_receipt_devino ledger_device_inode;
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_lock_owner {
    struct dcent_receipt_slice transaction_id;
    struct dcent_receipt_slice boot_id;
    uint32_t pid;
    uint64_t starttime;
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_lock_anchor {
    bool initialized;
    struct dcent_receipt_id transaction_id;
    struct dcent_receipt_id boot_id;
    uint32_t pid;
    uint64_t starttime;
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_resource_intent {
    struct dcent_receipt_digest binding_sha256;
    struct dcent_receipt_slice transaction_id;
    enum dcent_receipt_resource_kind kind;
    struct dcent_receipt_slice resource_id;
    enum dcent_receipt_provenance provenance;
    struct dcent_receipt_slice identity_a;
    struct dcent_receipt_slice identity_b;
    struct dcent_receipt_slice identity_c;
    struct dcent_receipt_evidence evidence;
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_resource_status {
    struct dcent_receipt_digest binding_sha256;
    struct dcent_receipt_slice transaction_id;
    enum dcent_receipt_resource_kind kind;
    struct dcent_receipt_slice resource_id;
    struct dcent_receipt_digest intent_sha256;
    enum dcent_receipt_resource_phase phase;
    uint32_t revision;
    uint32_t ledger_generation;
    struct dcent_receipt_digest previous_status_sha256;
    enum dcent_receipt_actor_kind actor_kind;
    struct dcent_receipt_slice actor_id;
    struct dcent_receipt_evidence evidence;
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_claim_intent {
    struct dcent_receipt_digest binding_sha256;
    struct dcent_receipt_slice transaction_id;
    struct dcent_receipt_slice claim_id;
    struct dcent_receipt_slice reconciler_boot_id;
    uint32_t reconciler_pid;
    uint64_t reconciler_starttime;
    struct dcent_receipt_devino reconciler_mount_namespace;
    struct dcent_receipt_slice maintenance_lock_path;
    struct dcent_receipt_devino maintenance_lock_device_inode;
    struct dcent_receipt_evidence evidence;
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_claim_status {
    struct dcent_receipt_digest claim_intent_sha256;
    enum dcent_receipt_claim_phase phase;
    uint32_t revision;
    uint32_t ledger_generation;
    struct dcent_receipt_digest previous_status_sha256;
    struct dcent_receipt_slice actor_id;
    struct dcent_receipt_digest quiescence_sha256;
    struct dcent_receipt_evidence evidence;
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_transaction_phase_status {
    struct dcent_receipt_digest binding_sha256;
    struct dcent_receipt_slice transaction_id;
    enum dcent_receipt_lock_phase phase;
    uint32_t revision;
    uint32_t ledger_generation;
    struct dcent_receipt_digest previous_status_sha256;
    enum dcent_receipt_actor_kind actor_kind;
    struct dcent_receipt_slice actor_id;
    struct dcent_receipt_evidence evidence;
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_resource_chain {
    bool initialized;
    enum dcent_receipt_resource_kind kind;
    struct dcent_receipt_id transaction_id;
    struct dcent_receipt_id resource_id;
    struct dcent_receipt_digest binding_sha256;
    unsigned char intent_sha256[DCENT_RECEIPT_SHA256_BYTES];
    unsigned char latest_status_sha256[DCENT_RECEIPT_SHA256_BYTES];
    enum dcent_receipt_resource_phase latest_phase;
    uint32_t revisions;
    uint32_t latest_ledger_generation;
    enum dcent_receipt_chain_authority authority;
    struct dcent_receipt_id reconciler_id;
};

struct dcent_receipt_claim_chain {
    bool initialized;
    struct dcent_receipt_id transaction_id;
    struct dcent_receipt_id claim_id;
    struct dcent_receipt_id reconciler_boot_id;
    uint32_t reconciler_pid;
    uint64_t reconciler_starttime;
    struct dcent_receipt_devino reconciler_mount_namespace;
    struct dcent_receipt_path maintenance_lock_path;
    struct dcent_receipt_devino maintenance_lock_device_inode;
    struct dcent_receipt_digest owner_death_evidence_sha256;
    struct dcent_receipt_digest binding_sha256;
    unsigned char intent_sha256[DCENT_RECEIPT_SHA256_BYTES];
    unsigned char latest_status_sha256[DCENT_RECEIPT_SHA256_BYTES];
    struct dcent_receipt_digest quiescence_sha256;
    enum dcent_receipt_claim_phase latest_phase;
    uint32_t revisions;
    uint32_t latest_ledger_generation;
    bool saw_reconciling;
};

struct dcent_receipt_transaction_phase_chain {
    bool initialized;
    struct dcent_receipt_id transaction_id;
    struct dcent_receipt_digest binding_sha256;
    unsigned char latest_status_sha256[DCENT_RECEIPT_SHA256_BYTES];
    enum dcent_receipt_lock_phase latest_phase;
    uint32_t revisions;
    uint32_t latest_ledger_generation;
};

enum dcent_receipt_resource_kind dcent_receipt_resource_kind_parse(
    const char *text);
const char *dcent_receipt_resource_kind_name(
    enum dcent_receipt_resource_kind value);
enum dcent_receipt_actor_kind dcent_receipt_actor_kind_parse(const char *text);
const char *dcent_receipt_actor_kind_name(enum dcent_receipt_actor_kind value);
enum dcent_receipt_evidence_type dcent_receipt_evidence_type_parse(
    const char *text);
const char *dcent_receipt_evidence_type_name(
    enum dcent_receipt_evidence_type value);

int dcent_receipt_parse_binding_abi1(const void *data, size_t size,
                                     struct dcent_receipt_binding *out);
int dcent_receipt_parse_lock_owner_v3(
    const void *data, size_t size, struct dcent_receipt_lock_owner *out);
int dcent_receipt_parse_resource_intent_abi1(
    const void *data, size_t size, struct dcent_receipt_resource_intent *out);
int dcent_receipt_parse_resource_status_abi1(
    const void *data, size_t size, struct dcent_receipt_resource_status *out);
int dcent_receipt_parse_claim_intent_abi1(
    const void *data, size_t size, struct dcent_receipt_claim_intent *out);
int dcent_receipt_parse_claim_status_abi1(
    const void *data, size_t size, struct dcent_receipt_claim_status *out);
int dcent_receipt_parse_transaction_phase_status_abi2(
    const void *data, size_t size,
    struct dcent_receipt_transaction_phase_status *out);

int dcent_receipt_binding_anchor_init(
    struct dcent_receipt_binding_anchor *anchor,
    const struct dcent_receipt_binding *binding);
int dcent_receipt_lock_anchor_init(
    struct dcent_receipt_lock_anchor *anchor,
    const struct dcent_receipt_lock_owner *owner);

int dcent_receipt_resource_chain_begin(
    struct dcent_receipt_resource_chain *chain,
    const struct dcent_receipt_binding_anchor *binding,
    const struct dcent_receipt_resource_intent *intent);
int dcent_receipt_resource_chain_add(
    struct dcent_receipt_resource_chain *chain,
    const struct dcent_receipt_resource_status *status);
int dcent_receipt_resource_chain_finish(
    const struct dcent_receipt_resource_chain *chain);

int dcent_receipt_claim_chain_begin(
    struct dcent_receipt_claim_chain *chain,
    const struct dcent_receipt_binding_anchor *binding,
    const struct dcent_receipt_claim_intent *intent);
int dcent_receipt_claim_chain_add(
    struct dcent_receipt_claim_chain *chain,
    const struct dcent_receipt_claim_status *status);
int dcent_receipt_claim_chain_finish(
    const struct dcent_receipt_claim_chain *chain);

int dcent_receipt_transaction_phase_chain_begin(
    struct dcent_receipt_transaction_phase_chain *chain,
    const struct dcent_receipt_binding_anchor *binding);
int dcent_receipt_transaction_phase_chain_add(
    struct dcent_receipt_transaction_phase_chain *chain,
    const struct dcent_receipt_transaction_phase_status *status);
int dcent_receipt_transaction_phase_chain_finish(
    const struct dcent_receipt_transaction_phase_chain *chain);

int dcent_receipt_ledger_validate_summary(
    const struct dcent_receipt_binding_anchor *binding,
    const struct dcent_receipt_resource_chain *resources, size_t resource_count,
    const struct dcent_receipt_claim_chain *claim, size_t aggregate_bytes);

#endif
