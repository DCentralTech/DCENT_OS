/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef DCENTOS_RECEIPT_PROJECTION_H
#define DCENTOS_RECEIPT_PROJECTION_H

#include "receipt_format.h"
#include "receipt_storage.h"

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define DCENT_RECEIPT_PROJECTION_MAX_STATE_BYTES (48U * 1024U)

/*
 * Internal, allocation-free ABI2 semantic engine.
 *
 * This layer consumes records that a descriptor scanner has parsed itself. It
 * proves one complete bounded event chronology, both surviving manifest
 * projections, and the pair-classified final delta. Historical head links
 * older than the surviving G-1/G pair are not yet reconstructed. It performs
 * no filesystem or process admission and MUST NOT be exposed as mutation
 * authority. Production mutation APIs must keep this header private, allocate
 * this large state outside bounded worker stacks, and return opaque sessions
 * that retain the admitted guard, maintenance lock, lease, namespace, and
 * tree descriptors.
 */

enum dcent_receipt_projection_result {
    DCENT_RECEIPT_PROJECTION_OK = 0,
    DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT,
    DCENT_RECEIPT_PROJECTION_FORMAT,
    DCENT_RECEIPT_PROJECTION_STORAGE,
    DCENT_RECEIPT_PROJECTION_LIMIT,
    DCENT_RECEIPT_PROJECTION_DUPLICATE_GENERATION,
    DCENT_RECEIPT_PROJECTION_GENERATION_GAP,
    DCENT_RECEIPT_PROJECTION_CHRONOLOGY,
    DCENT_RECEIPT_PROJECTION_MANIFEST_MISMATCH,
};

enum dcent_receipt_projection_event_kind {
    DCENT_RECEIPT_PROJECTION_EVENT_NONE = 0,
    DCENT_RECEIPT_PROJECTION_EVENT_RESOURCE,
    DCENT_RECEIPT_PROJECTION_EVENT_CLAIM,
    DCENT_RECEIPT_PROJECTION_EVENT_PHASE,
};

struct dcent_receipt_projection_status {
    uint32_t ledger_generation;
    uint32_t phase;
    enum dcent_receipt_actor_kind actor_kind;
    unsigned char record_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

struct dcent_receipt_projection_resource {
    bool initialized;
    bool finished;
    struct dcent_receipt_resource_chain chain;
    struct dcent_receipt_projection_status
        statuses[DCENT_RECEIPT_MAX_REVISIONS];
};

struct dcent_receipt_projection_claim {
    bool initialized;
    bool finished;
    struct dcent_receipt_claim_chain chain;
    struct dcent_receipt_projection_status
        statuses[DCENT_RECEIPT_MAX_REVISIONS];
};

struct dcent_receipt_projection_phase {
    bool finished;
    struct dcent_receipt_transaction_phase_chain chain;
    struct dcent_receipt_projection_status
        statuses[DCENT_RECEIPT_MAX_PHASE_REVISIONS];
    struct dcent_receipt_id reconciler_id;
};

struct dcent_receipt_projection_event {
    enum dcent_receipt_projection_event_kind kind;
    uint8_t object_index;
    uint8_t revision;
};

struct dcent_receipt_projection {
    bool initialized;
    struct dcent_receipt_binding_anchor binding;
    struct dcent_receipt_lock_anchor lock;
    struct dcent_receipt_storage_seal seal;
    struct dcent_receipt_storage_head banks[2];
    struct dcent_receipt_storage_manifest_pair pair;
    size_t resource_count;
    struct dcent_receipt_projection_resource
        resources[DCENT_RECEIPT_MAX_RESOURCES];
    struct dcent_receipt_projection_claim claim;
    struct dcent_receipt_projection_phase phase;
    struct dcent_receipt_projection_event
        events[DCENT_RECEIPT_MAX_LEDGER_GENERATION + 1U];
};

_Static_assert(sizeof(struct dcent_receipt_projection) <=
                   DCENT_RECEIPT_PROJECTION_MAX_STATE_BYTES,
               "receipt projection state exceeds its embedded memory budget");

struct dcent_receipt_projection_summary {
    bool initialized;
    uint32_t generation;
    size_t event_count;
    size_t resource_count;
    bool claim_present;
    enum dcent_receipt_chain_authority authority;
    enum dcent_receipt_lock_phase transaction_phase;
    unsigned char current_head_sha256[DCENT_RECEIPT_SHA256_BYTES];
};

enum dcent_receipt_projection_result dcent_receipt_projection_init_abi2(
    struct dcent_receipt_projection *projection,
    const struct dcent_receipt_binding_anchor *binding,
    const struct dcent_receipt_lock_anchor *lock,
    const struct dcent_receipt_storage_seal *seal,
    const struct dcent_receipt_storage_head *bank0,
    const struct dcent_receipt_storage_head *bank1);

enum dcent_receipt_projection_result dcent_receipt_projection_resource_begin(
    struct dcent_receipt_projection *projection,
    const struct dcent_receipt_resource_intent *intent, size_t *resource_slot);
enum dcent_receipt_projection_result dcent_receipt_projection_resource_add(
    struct dcent_receipt_projection *projection, size_t resource_slot,
    const struct dcent_receipt_resource_status *status);
enum dcent_receipt_projection_result dcent_receipt_projection_resource_finish(
    struct dcent_receipt_projection *projection, size_t resource_slot);

enum dcent_receipt_projection_result dcent_receipt_projection_claim_begin(
    struct dcent_receipt_projection *projection,
    const struct dcent_receipt_claim_intent *intent);
enum dcent_receipt_projection_result dcent_receipt_projection_claim_add(
    struct dcent_receipt_projection *projection,
    const struct dcent_receipt_claim_status *status);
enum dcent_receipt_projection_result dcent_receipt_projection_claim_finish(
    struct dcent_receipt_projection *projection);

enum dcent_receipt_projection_result dcent_receipt_projection_phase_add(
    struct dcent_receipt_projection *projection,
    const struct dcent_receipt_transaction_phase_status *status);
enum dcent_receipt_projection_result dcent_receipt_projection_phase_finish(
    struct dcent_receipt_projection *projection);

enum dcent_receipt_projection_result dcent_receipt_projection_finalize_abi2(
    const struct dcent_receipt_projection *projection, size_t aggregate_bytes,
    struct dcent_receipt_projection_summary *out);

const char *dcent_receipt_projection_result_name(
    enum dcent_receipt_projection_result result);

#endif
