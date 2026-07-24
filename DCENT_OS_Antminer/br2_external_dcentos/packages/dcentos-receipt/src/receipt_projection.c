/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_projection.h"

#include <string.h>

static bool bytes_equal(const unsigned char *left,
                        const unsigned char *right, size_t size)
{
    return memcmp(left, right, size) == 0;
}

static bool id_valid(const struct dcent_receipt_id *value)
{
    return value != NULL && value->size != 0U &&
           value->size <= DCENT_RECEIPT_MAX_ID;
}

static bool id_equal(const struct dcent_receipt_id *left,
                     const struct dcent_receipt_id *right)
{
    return id_valid(left) && id_valid(right) && left->size == right->size &&
           bytes_equal(left->bytes, right->bytes, left->size);
}

static bool id_equal_slice(const struct dcent_receipt_id *left,
                           struct dcent_receipt_slice right)
{
    return id_valid(left) && right.data != NULL && right.size == left->size &&
           bytes_equal(left->bytes, right.data, right.size);
}

static bool devino_equal(struct dcent_receipt_devino left,
                         struct dcent_receipt_devino right)
{
    return left.device == right.device && left.inode == right.inode;
}

static bool identity_valid(
    const struct dcent_receipt_binding_anchor *binding,
    const struct dcent_receipt_lock_anchor *lock,
    const struct dcent_receipt_storage_seal *seal)
{
    if (binding == NULL || lock == NULL || seal == NULL ||
        !binding->initialized || !lock->initialized || !seal->initialized ||
        binding->owner_pid == 0U || binding->owner_starttime == 0U ||
        binding->storage_mount_id == 0U || seal->storage_mount_id == 0U ||
        !binding->transaction_lock_owner_sha256.present)
        return false;
    return id_equal(&binding->transaction_id, &lock->transaction_id) &&
           id_equal(&binding->transaction_id, &seal->transaction_id) &&
           id_equal(&binding->boot_id, &lock->boot_id) &&
           id_equal(&binding->boot_id, &seal->boot_id) &&
           binding->owner_pid == lock->pid &&
           binding->owner_starttime == lock->starttime &&
           devino_equal(binding->acquisition_guard_device_inode,
                        seal->acquisition_guard_device_inode) &&
           devino_equal(binding->transaction_lock_device_inode,
                        seal->transaction_lock_device_inode) &&
           devino_equal(binding->transaction_lock_owner_device_inode,
                        seal->transaction_lock_owner_device_inode) &&
           devino_equal(binding->ledger_device_inode,
                        seal->ledger_device_inode) &&
           binding->storage_mount_id == seal->storage_mount_id &&
           bytes_equal(binding->transaction_lock_owner_sha256.bytes,
                       lock->record_sha256,
                       DCENT_RECEIPT_SHA256_BYTES) &&
           bytes_equal(binding->transaction_lock_owner_sha256.bytes,
                       seal->transaction_lock_owner_sha256,
                       DCENT_RECEIPT_SHA256_BYTES) &&
           bytes_equal(binding->record_sha256, seal->binding_sha256,
                       DCENT_RECEIPT_SHA256_BYTES);
}

static enum dcent_receipt_projection_result format_result(int result)
{
    return result == DCENT_RECEIPT_FORMAT_LIMIT
               ? DCENT_RECEIPT_PROJECTION_LIMIT
               : DCENT_RECEIPT_PROJECTION_FORMAT;
}

static bool event_available(const struct dcent_receipt_projection *projection,
                            uint32_t generation)
{
    return generation != 0U &&
           generation <= projection->pair.current_generation &&
           generation <= DCENT_RECEIPT_MAX_LEDGER_GENERATION &&
           projection->events[generation].kind ==
               DCENT_RECEIPT_PROJECTION_EVENT_NONE;
}

static bool mutable_projection_valid(
    const struct dcent_receipt_projection *projection)
{
    return projection != NULL && projection->initialized &&
           projection->resource_count <= DCENT_RECEIPT_MAX_RESOURCES &&
           projection->pair.initialized &&
           projection->pair.current_generation <=
               DCENT_RECEIPT_MAX_LEDGER_GENERATION;
}

static bool delta_equal(const struct dcent_receipt_storage_delta *left,
                        const struct dcent_receipt_storage_delta *right)
{
    return left->initialized == right->initialized &&
           left->kind == right->kind &&
           left->resource_kind == right->resource_kind &&
           ((!id_valid(&left->object_id) && !id_valid(&right->object_id)) ||
            id_equal(&left->object_id, &right->object_id)) &&
           left->previous_revision == right->previous_revision &&
           left->target_revision == right->target_revision &&
           left->previous_phase == right->previous_phase &&
           left->target_phase == right->target_phase;
}

static bool pair_equal(
    const struct dcent_receipt_storage_manifest_pair *left,
    const struct dcent_receipt_storage_manifest_pair *right)
{
    return left->initialized == right->initialized &&
           left->genesis == right->genesis &&
           left->previous_bank == right->previous_bank &&
           left->current_bank == right->current_bank &&
           left->current_generation == right->current_generation &&
           bytes_equal(left->current_head_sha256,
                       right->current_head_sha256,
                       DCENT_RECEIPT_SHA256_BYTES) &&
           delta_equal(&left->delta, &right->delta);
}

static void store_status(struct dcent_receipt_projection_status *out,
                         uint32_t generation, uint32_t phase,
                         enum dcent_receipt_actor_kind actor_kind,
                         const unsigned char digest[DCENT_RECEIPT_SHA256_BYTES])
{
    out->ledger_generation = generation;
    out->phase = phase;
    out->actor_kind = actor_kind;
    memcpy(out->record_sha256, digest, DCENT_RECEIPT_SHA256_BYTES);
}

enum dcent_receipt_projection_result dcent_receipt_projection_init_abi2(
    struct dcent_receipt_projection *projection,
    const struct dcent_receipt_binding_anchor *binding,
    const struct dcent_receipt_lock_anchor *lock,
    const struct dcent_receipt_storage_seal *seal,
    const struct dcent_receipt_storage_head *bank0,
    const struct dcent_receipt_storage_head *bank1)
{
    struct dcent_receipt_binding_anchor binding_copy;
    struct dcent_receipt_lock_anchor lock_copy;
    struct dcent_receipt_storage_seal seal_copy;
    struct dcent_receipt_storage_head bank_copies[2];
    struct dcent_receipt_storage_manifest_pair pair;
    struct dcent_receipt_projection_phase phase;

    if (projection == NULL || binding == NULL || lock == NULL || seal == NULL ||
        bank0 == NULL || bank1 == NULL)
        return DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT;
    if (!identity_valid(binding, lock, seal))
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    memset(&pair, 0, sizeof(pair));
    if (dcent_receipt_storage_validate_manifest_pair_abi2(
            seal, bank0, bank1, &pair) != DCENT_RECEIPT_STORAGE_OK)
        return DCENT_RECEIPT_PROJECTION_STORAGE;
    memset(&phase, 0, sizeof(phase));
    if (dcent_receipt_transaction_phase_chain_begin(&phase.chain, binding) !=
        DCENT_RECEIPT_FORMAT_OK)
        return DCENT_RECEIPT_PROJECTION_FORMAT;

    /* Permit explicit in-place reinitialization without zeroing aliased input. */
    binding_copy = *binding;
    lock_copy = *lock;
    seal_copy = *seal;
    bank_copies[0] = *bank0;
    bank_copies[1] = *bank1;
    memset(projection, 0, sizeof(*projection));
    projection->binding = binding_copy;
    projection->lock = lock_copy;
    projection->seal = seal_copy;
    projection->banks[0] = bank_copies[0];
    projection->banks[1] = bank_copies[1];
    projection->pair = pair;
    projection->phase = phase;
    projection->initialized = true;
    return DCENT_RECEIPT_PROJECTION_OK;
}

enum dcent_receipt_projection_result dcent_receipt_projection_resource_begin(
    struct dcent_receipt_projection *projection,
    const struct dcent_receipt_resource_intent *intent, size_t *resource_slot)
{
    struct dcent_receipt_projection_resource resource;
    size_t index;
    int result;

    if (!mutable_projection_valid(projection) || intent == NULL ||
        resource_slot == NULL)
        return DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT;
    if (projection->resource_count >= DCENT_RECEIPT_MAX_RESOURCES)
        return DCENT_RECEIPT_PROJECTION_LIMIT;
    for (index = 0U; index < projection->resource_count; index++) {
        if (projection->resources[index].chain.kind == intent->kind &&
            id_equal_slice(&projection->resources[index].chain.resource_id,
                           intent->resource_id))
            return DCENT_RECEIPT_PROJECTION_FORMAT;
    }

    memset(&resource, 0, sizeof(resource));
    result = dcent_receipt_resource_chain_begin(
        &resource.chain, &projection->binding, intent);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return format_result(result);
    resource.initialized = true;
    index = projection->resource_count;
    projection->resources[index] = resource;
    projection->resource_count++;
    *resource_slot = index;
    return DCENT_RECEIPT_PROJECTION_OK;
}

enum dcent_receipt_projection_result dcent_receipt_projection_resource_add(
    struct dcent_receipt_projection *projection, size_t resource_slot,
    const struct dcent_receipt_resource_status *status)
{
    struct dcent_receipt_projection_resource next;
    struct dcent_receipt_projection_event event;
    uint32_t revision;
    int result;

    if (!mutable_projection_valid(projection) || status == NULL ||
        resource_slot >= projection->resource_count ||
        !projection->resources[resource_slot].initialized)
        return DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT;
    if (projection->resources[resource_slot].finished)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    if (!event_available(projection, status->ledger_generation))
        return status->ledger_generation == 0U ||
                       status->ledger_generation >
                           projection->pair.current_generation
                   ? DCENT_RECEIPT_PROJECTION_LIMIT
                   : DCENT_RECEIPT_PROJECTION_DUPLICATE_GENERATION;

    next = projection->resources[resource_slot];
    result = dcent_receipt_resource_chain_add(&next.chain, status);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return format_result(result);
    revision = next.chain.revisions;
    if (revision == 0U || revision > DCENT_RECEIPT_MAX_REVISIONS)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    store_status(&next.statuses[revision - 1U], status->ledger_generation,
                 (uint32_t)status->phase, status->actor_kind,
                 status->record_sha256);
    memset(&event, 0, sizeof(event));
    event.kind = DCENT_RECEIPT_PROJECTION_EVENT_RESOURCE;
    event.object_index = (uint8_t)resource_slot;
    event.revision = (uint8_t)revision;
    projection->resources[resource_slot] = next;
    projection->events[status->ledger_generation] = event;
    return DCENT_RECEIPT_PROJECTION_OK;
}

enum dcent_receipt_projection_result dcent_receipt_projection_resource_finish(
    struct dcent_receipt_projection *projection, size_t resource_slot)
{
    if (!mutable_projection_valid(projection) ||
        resource_slot >= projection->resource_count ||
        !projection->resources[resource_slot].initialized)
        return DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT;
    if (projection->resources[resource_slot].finished)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    if (dcent_receipt_resource_chain_finish(
            &projection->resources[resource_slot].chain) !=
        DCENT_RECEIPT_FORMAT_OK)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    projection->resources[resource_slot].finished = true;
    return DCENT_RECEIPT_PROJECTION_OK;
}

enum dcent_receipt_projection_result dcent_receipt_projection_claim_begin(
    struct dcent_receipt_projection *projection,
    const struct dcent_receipt_claim_intent *intent)
{
    struct dcent_receipt_projection_claim claim;
    int result;

    if (!mutable_projection_valid(projection) || intent == NULL)
        return DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT;
    if (projection->claim.initialized)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    if (id_valid(&projection->phase.reconciler_id) &&
        !id_equal_slice(&projection->phase.reconciler_id, intent->claim_id))
        return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
    memset(&claim, 0, sizeof(claim));
    result = dcent_receipt_claim_chain_begin(
        &claim.chain, &projection->binding, intent);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return format_result(result);
    claim.initialized = true;
    projection->claim = claim;
    return DCENT_RECEIPT_PROJECTION_OK;
}

enum dcent_receipt_projection_result dcent_receipt_projection_claim_add(
    struct dcent_receipt_projection *projection,
    const struct dcent_receipt_claim_status *status)
{
    struct dcent_receipt_projection_claim next;
    struct dcent_receipt_projection_event event;
    uint32_t revision;
    int result;

    if (!mutable_projection_valid(projection) || status == NULL ||
        !projection->claim.initialized)
        return DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT;
    if (projection->claim.finished)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    if (!event_available(projection, status->ledger_generation))
        return status->ledger_generation == 0U ||
                       status->ledger_generation >
                           projection->pair.current_generation
                   ? DCENT_RECEIPT_PROJECTION_LIMIT
                   : DCENT_RECEIPT_PROJECTION_DUPLICATE_GENERATION;

    next = projection->claim;
    result = dcent_receipt_claim_chain_add(&next.chain, status);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return format_result(result);
    revision = next.chain.revisions;
    if (revision == 0U || revision > DCENT_RECEIPT_MAX_REVISIONS)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    store_status(&next.statuses[revision - 1U], status->ledger_generation,
                 (uint32_t)status->phase,
                 DCENT_RECEIPT_ACTOR_RECONCILER,
                 status->record_sha256);
    memset(&event, 0, sizeof(event));
    event.kind = DCENT_RECEIPT_PROJECTION_EVENT_CLAIM;
    event.revision = (uint8_t)revision;
    projection->claim = next;
    projection->events[status->ledger_generation] = event;
    return DCENT_RECEIPT_PROJECTION_OK;
}

enum dcent_receipt_projection_result dcent_receipt_projection_claim_finish(
    struct dcent_receipt_projection *projection)
{
    if (!mutable_projection_valid(projection) ||
        !projection->claim.initialized)
        return DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT;
    if (projection->claim.finished)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    if (dcent_receipt_claim_chain_finish(&projection->claim.chain) !=
        DCENT_RECEIPT_FORMAT_OK)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    projection->claim.finished = true;
    return DCENT_RECEIPT_PROJECTION_OK;
}

enum dcent_receipt_projection_result dcent_receipt_projection_phase_add(
    struct dcent_receipt_projection *projection,
    const struct dcent_receipt_transaction_phase_status *status)
{
    struct dcent_receipt_projection_phase next;
    struct dcent_receipt_projection_event event;
    uint32_t revision;
    int result;

    if (!mutable_projection_valid(projection) || status == NULL)
        return DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT;
    if (projection->phase.finished)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    if (!event_available(projection, status->ledger_generation))
        return status->ledger_generation == 0U ||
                       status->ledger_generation >
                           projection->pair.current_generation
                   ? DCENT_RECEIPT_PROJECTION_LIMIT
                   : DCENT_RECEIPT_PROJECTION_DUPLICATE_GENERATION;

    next = projection->phase;
    result = dcent_receipt_transaction_phase_chain_add(&next.chain, status);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return format_result(result);
    if (status->actor_kind == DCENT_RECEIPT_ACTOR_RECONCILER) {
        if (id_valid(&next.reconciler_id) &&
            !id_equal_slice(&next.reconciler_id, status->actor_id))
            return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
        if (!id_valid(&next.reconciler_id)) {
            next.reconciler_id.size = status->actor_id.size;
            memcpy(next.reconciler_id.bytes, status->actor_id.data,
                   status->actor_id.size);
        }
        if (projection->claim.initialized &&
            !id_equal(&next.reconciler_id,
                      &projection->claim.chain.claim_id))
            return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
    }
    revision = next.chain.revisions;
    if (revision == 0U || revision > DCENT_RECEIPT_MAX_PHASE_REVISIONS)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    store_status(&next.statuses[revision - 1U], status->ledger_generation,
                 (uint32_t)status->phase, status->actor_kind,
                 status->record_sha256);
    memset(&event, 0, sizeof(event));
    event.kind = DCENT_RECEIPT_PROJECTION_EVENT_PHASE;
    event.revision = (uint8_t)revision;
    projection->phase = next;
    projection->events[status->ledger_generation] = event;
    return DCENT_RECEIPT_PROJECTION_OK;
}

enum dcent_receipt_projection_result dcent_receipt_projection_phase_finish(
    struct dcent_receipt_projection *projection)
{
    if (!mutable_projection_valid(projection))
        return DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT;
    if (projection->phase.finished)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    if (dcent_receipt_transaction_phase_chain_finish(
            &projection->phase.chain) != DCENT_RECEIPT_FORMAT_OK)
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    projection->phase.finished = true;
    return DCENT_RECEIPT_PROJECTION_OK;
}

static uint32_t latest_revision_before(
    const struct dcent_receipt_projection_status *statuses,
    uint32_t revisions, uint32_t generation)
{
    uint32_t revision;
    uint32_t latest = 0U;

    for (revision = 0U; revision < revisions; revision++) {
        if (statuses[revision].ledger_generation <= generation)
            latest = revision + 1U;
    }
    return latest;
}

static const struct dcent_receipt_storage_resource_head *find_head_resource(
    const struct dcent_receipt_storage_head *head,
    const struct dcent_receipt_projection_resource *resource)
{
    size_t index;

    for (index = 0U; index < head->resource_count; index++) {
        if (head->resources[index].kind == resource->chain.kind &&
            id_equal(&head->resources[index].resource_id,
                     &resource->chain.resource_id))
            return &head->resources[index];
    }
    return NULL;
}

static bool head_matches_prefix(
    const struct dcent_receipt_projection *projection,
    const struct dcent_receipt_storage_head *head, uint32_t generation)
{
    size_t index;
    size_t present_resources = 0U;
    uint32_t phase_revision;
    uint32_t claim_revision;

    if (head->generation != generation)
        return false;
    for (index = 0U; index < projection->resource_count; index++) {
        const struct dcent_receipt_projection_resource *resource =
            &projection->resources[index];
        const struct dcent_receipt_storage_resource_head *row;
        uint32_t revision = latest_revision_before(
            resource->statuses, resource->chain.revisions, generation);

        if (revision == 0U)
            continue;
        present_resources++;
        row = find_head_resource(head, resource);
        if (row == NULL ||
            !bytes_equal(row->intent_sha256, resource->chain.intent_sha256,
                         DCENT_RECEIPT_SHA256_BYTES) ||
            row->status_revision != revision ||
            !bytes_equal(row->status_sha256,
                         resource->statuses[revision - 1U].record_sha256,
                         DCENT_RECEIPT_SHA256_BYTES))
            return false;
    }
    if (present_resources != head->resource_count)
        return false;

    claim_revision = projection->claim.initialized
                         ? latest_revision_before(
                               projection->claim.statuses,
                               projection->claim.chain.revisions, generation)
                         : 0U;
    if (claim_revision == 0U) {
        if (head->claim_present ||
            head->authority != DCENT_RECEIPT_AUTHORITY_OWNER ||
            !id_equal(&head->authority_id,
                      &projection->binding.transaction_id))
            return false;
    } else if (!head->claim_present ||
               head->authority != DCENT_RECEIPT_AUTHORITY_RECONCILER ||
               !id_equal(&head->claim_id,
                         &projection->claim.chain.claim_id) ||
               !id_equal(&head->authority_id,
                         &projection->claim.chain.claim_id) ||
               !bytes_equal(head->claim_intent_sha256,
                            projection->claim.chain.intent_sha256,
                            DCENT_RECEIPT_SHA256_BYTES) ||
               head->claim_status_revision != claim_revision ||
               !bytes_equal(
                   head->claim_status_sha256,
                   projection->claim.statuses[claim_revision - 1U]
                       .record_sha256,
                   DCENT_RECEIPT_SHA256_BYTES)) {
        return false;
    }

    phase_revision = latest_revision_before(
        projection->phase.statuses, projection->phase.chain.revisions,
        generation);
    if (head->transaction_phase_revision != phase_revision)
        return false;
    if (phase_revision == 0U) {
        return head->transaction_phase == DCENT_RECEIPT_LOCK_ACTIVE &&
               !head->transaction_phase_status_present;
    }
    return head->transaction_phase_status_present &&
           head->transaction_phase ==
               (enum dcent_receipt_lock_phase)
                   projection->phase.statuses[phase_revision - 1U].phase &&
           bytes_equal(
               head->transaction_phase_status_sha256,
               projection->phase.statuses[phase_revision - 1U].record_sha256,
               DCENT_RECEIPT_SHA256_BYTES);
}

static bool all_resources_released(
    const enum dcent_receipt_resource_phase
        phases[DCENT_RECEIPT_MAX_RESOURCES],
    size_t count)
{
    size_t index;

    for (index = 0U; index < count; index++) {
        if (phases[index] != DCENT_RECEIPT_RESOURCE_RELEASED)
            return false;
    }
    return true;
}

static bool all_present_resources_released(
    const enum dcent_receipt_resource_phase
        phases[DCENT_RECEIPT_MAX_RESOURCES],
    size_t count)
{
    size_t index;

    for (index = 0U; index < count; index++) {
        if (phases[index] != DCENT_RECEIPT_RESOURCE_INVALID &&
            phases[index] != DCENT_RECEIPT_RESOURCE_RELEASED)
            return false;
    }
    return true;
}

static enum dcent_receipt_projection_result validate_chronology(
    const struct dcent_receipt_projection *projection,
    uint32_t current_generation)
{
    enum dcent_receipt_resource_phase
        resource_phases[DCENT_RECEIPT_MAX_RESOURCES];
    enum dcent_receipt_claim_phase claim_phase = DCENT_RECEIPT_CLAIM_INVALID;
    enum dcent_receipt_lock_phase transaction_phase =
        DCENT_RECEIPT_LOCK_ACTIVE;
    bool claim_present = false;
    bool claim_terminal = false;
    uint32_t generation;

    memset(resource_phases, 0, sizeof(resource_phases));
    for (generation = 1U; generation <= current_generation;
         generation++) {
        const struct dcent_receipt_projection_event *event =
            &projection->events[generation];

        if (event->kind == DCENT_RECEIPT_PROJECTION_EVENT_NONE)
            return DCENT_RECEIPT_PROJECTION_GENERATION_GAP;
        if (claim_terminal ||
            transaction_phase == DCENT_RECEIPT_LOCK_ENV_COMMITTED)
            return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;

        if (event->kind == DCENT_RECEIPT_PROJECTION_EVENT_RESOURCE) {
            const struct dcent_receipt_projection_resource *resource;
            const struct dcent_receipt_projection_status *status;
            enum dcent_receipt_resource_phase resource_phase;

            if (event->object_index >= projection->resource_count ||
                event->revision == 0U ||
                event->revision > DCENT_RECEIPT_MAX_REVISIONS)
                return DCENT_RECEIPT_PROJECTION_FORMAT;
            resource = &projection->resources[event->object_index];
            status = &resource->statuses[event->revision - 1U];
            if (status->ledger_generation != generation)
                return DCENT_RECEIPT_PROJECTION_FORMAT;
            if (status->actor_kind == DCENT_RECEIPT_ACTOR_OWNER) {
                if (claim_present)
                    return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
            } else if (status->actor_kind ==
                       DCENT_RECEIPT_ACTOR_RECONCILER) {
                if (!claim_present ||
                    claim_phase != DCENT_RECEIPT_CLAIM_RECONCILING ||
                    !id_equal(&resource->chain.reconciler_id,
                              &projection->claim.chain.claim_id))
                    return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
            } else {
                return DCENT_RECEIPT_PROJECTION_FORMAT;
            }
            if (transaction_phase == DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED)
                return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
            resource_phase =
                (enum dcent_receipt_resource_phase)status->phase;
            if (transaction_phase == DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED &&
                resource_phase != DCENT_RECEIPT_RESOURCE_RELEASE_PENDING &&
                resource_phase != DCENT_RECEIPT_RESOURCE_RELEASED &&
                resource_phase != DCENT_RECEIPT_RESOURCE_CONFLICT)
                return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
            resource_phases[event->object_index] = resource_phase;
        } else if (event->kind == DCENT_RECEIPT_PROJECTION_EVENT_CLAIM) {
            const struct dcent_receipt_projection_status *status;

            if (!projection->claim.initialized || event->revision == 0U ||
                event->revision > DCENT_RECEIPT_MAX_REVISIONS)
                return DCENT_RECEIPT_PROJECTION_FORMAT;
            status = &projection->claim.statuses[event->revision - 1U];
            if (status->ledger_generation != generation)
                return DCENT_RECEIPT_PROJECTION_FORMAT;
            if (event->revision == 1U) {
                if (claim_present ||
                    transaction_phase != DCENT_RECEIPT_LOCK_ACTIVE)
                    return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
                claim_present = true;
            } else if (!claim_present) {
                return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
            }
            claim_phase = (enum dcent_receipt_claim_phase)status->phase;
            if (claim_phase == DCENT_RECEIPT_CLAIM_COMPLETE) {
                if (transaction_phase !=
                        DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED ||
                    !all_resources_released(resource_phases,
                                            projection->resource_count))
                    return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
                claim_terminal = true;
            } else if (claim_phase == DCENT_RECEIPT_CLAIM_BLOCKED) {
                claim_terminal = true;
            }
        } else if (event->kind == DCENT_RECEIPT_PROJECTION_EVENT_PHASE) {
            const struct dcent_receipt_projection_status *status;

            if (event->revision == 0U ||
                event->revision > DCENT_RECEIPT_MAX_PHASE_REVISIONS)
                return DCENT_RECEIPT_PROJECTION_FORMAT;
            status = &projection->phase.statuses[event->revision - 1U];
            if (status->ledger_generation != generation)
                return DCENT_RECEIPT_PROJECTION_FORMAT;
            if (status->actor_kind == DCENT_RECEIPT_ACTOR_OWNER) {
                if (claim_present)
                    return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
            } else if (status->actor_kind ==
                       DCENT_RECEIPT_ACTOR_RECONCILER) {
                if (!claim_present ||
                    claim_phase != DCENT_RECEIPT_CLAIM_RECONCILING ||
                    (id_valid(&projection->phase.reconciler_id) &&
                     !id_equal(&projection->phase.reconciler_id,
                               &projection->claim.chain.claim_id)) ||
                    status->phase !=
                        (uint32_t)DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED)
                    return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
            } else {
                return DCENT_RECEIPT_PROJECTION_FORMAT;
            }
            if ((status->phase ==
                     (uint32_t)DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED ||
                 status->phase ==
                     (uint32_t)DCENT_RECEIPT_LOCK_ENV_COMMITTED) &&
                !all_present_resources_released(
                    resource_phases, projection->resource_count))
                return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
            transaction_phase =
                (enum dcent_receipt_lock_phase)status->phase;
        } else {
            return DCENT_RECEIPT_PROJECTION_FORMAT;
        }
    }
    return DCENT_RECEIPT_PROJECTION_OK;
}

static bool final_event_matches_delta(
    const struct dcent_receipt_projection *projection,
    const struct dcent_receipt_storage_manifest_pair *pair)
{
    const struct dcent_receipt_projection_event *event;
    const struct dcent_receipt_storage_delta *delta = &pair->delta;
    uint32_t generation = pair->current_generation;

    if (generation == 0U)
        return pair->genesis;
    event = &projection->events[generation];
    if (event->kind == DCENT_RECEIPT_PROJECTION_EVENT_RESOURCE) {
        const struct dcent_receipt_projection_resource *resource =
            &projection->resources[event->object_index];
        enum dcent_receipt_storage_delta_kind kind =
            event->revision == 1U
                ? DCENT_RECEIPT_STORAGE_DELTA_RESOURCE_ADD
                : DCENT_RECEIPT_STORAGE_DELTA_RESOURCE_ADVANCE;

        return delta->kind == kind &&
               delta->resource_kind == resource->chain.kind &&
               id_equal(&delta->object_id, &resource->chain.resource_id) &&
               delta->target_revision == event->revision &&
               delta->previous_revision ==
                   (event->revision == 1U ? 0U : event->revision - 1U);
    }
    if (event->kind == DCENT_RECEIPT_PROJECTION_EVENT_CLAIM) {
        enum dcent_receipt_storage_delta_kind kind =
            event->revision == 1U ? DCENT_RECEIPT_STORAGE_DELTA_CLAIM_ADD
                                  : DCENT_RECEIPT_STORAGE_DELTA_CLAIM_ADVANCE;

        return delta->kind == kind &&
               id_equal(&delta->object_id,
                        &projection->claim.chain.claim_id) &&
               delta->target_revision == event->revision &&
               delta->previous_revision ==
                   (event->revision == 1U ? 0U : event->revision - 1U);
    }
    if (event->kind == DCENT_RECEIPT_PROJECTION_EVENT_PHASE) {
        enum dcent_receipt_lock_phase target =
            (enum dcent_receipt_lock_phase)
                projection->phase.statuses[event->revision - 1U].phase;
        enum dcent_receipt_lock_phase previous =
            event->revision == 1U
                ? DCENT_RECEIPT_LOCK_ACTIVE
                : (enum dcent_receipt_lock_phase)
                      projection->phase.statuses[event->revision - 2U].phase;

        return delta->kind == DCENT_RECEIPT_STORAGE_DELTA_PHASE_ADVANCE &&
               delta->target_revision == event->revision &&
               delta->previous_revision == event->revision - 1U &&
               delta->target_phase == target &&
               delta->previous_phase == previous;
    }
    return false;
}

enum dcent_receipt_projection_result dcent_receipt_projection_finalize_abi2(
    const struct dcent_receipt_projection *projection, size_t aggregate_bytes,
    struct dcent_receipt_projection_summary *out)
{
    struct dcent_receipt_projection_summary parsed;
    struct dcent_receipt_storage_manifest_pair pair;
    const struct dcent_receipt_storage_head *current;
    const struct dcent_receipt_storage_head *previous;
    enum dcent_receipt_projection_result result;
    size_t index;

    if (projection == NULL || out == NULL || !projection->initialized)
        return DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT;
    if (aggregate_bytes > DCENT_RECEIPT_MAX_LEDGER)
        return DCENT_RECEIPT_PROJECTION_LIMIT;
    if (projection->resource_count > DCENT_RECEIPT_MAX_RESOURCES ||
        projection->events[0].kind != DCENT_RECEIPT_PROJECTION_EVENT_NONE ||
        !identity_valid(&projection->binding, &projection->lock,
                        &projection->seal))
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    memset(&pair, 0, sizeof(pair));
    if (dcent_receipt_storage_validate_manifest_pair_abi2(
            &projection->seal, &projection->banks[0], &projection->banks[1],
            &pair) != DCENT_RECEIPT_STORAGE_OK)
        return DCENT_RECEIPT_PROJECTION_STORAGE;
    if (!pair_equal(&projection->pair, &pair))
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    for (index = (size_t)pair.current_generation + 1U;
         index <= DCENT_RECEIPT_MAX_LEDGER_GENERATION; index++) {
        if (projection->events[index].kind !=
            DCENT_RECEIPT_PROJECTION_EVENT_NONE)
            return DCENT_RECEIPT_PROJECTION_FORMAT;
    }
    if (!projection->phase.finished ||
        (projection->claim.initialized && !projection->claim.finished))
        return DCENT_RECEIPT_PROJECTION_FORMAT;
    for (index = 0U; index < projection->resource_count; index++) {
        if (!projection->resources[index].finished)
            return DCENT_RECEIPT_PROJECTION_FORMAT;
        if (projection->resources[index].chain.authority ==
                DCENT_RECEIPT_AUTHORITY_RECONCILER &&
            (!projection->claim.initialized ||
             !id_equal(&projection->resources[index].chain.reconciler_id,
                       &projection->claim.chain.claim_id)))
            return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;
    }
    if (id_valid(&projection->phase.reconciler_id) &&
        (!projection->claim.initialized ||
         !id_equal(&projection->phase.reconciler_id,
                   &projection->claim.chain.claim_id)))
        return DCENT_RECEIPT_PROJECTION_CHRONOLOGY;

    result = validate_chronology(projection, pair.current_generation);
    if (result != DCENT_RECEIPT_PROJECTION_OK)
        return result;
    current = &projection->banks[pair.current_bank];
    previous = &projection->banks[pair.previous_bank];
    if (!head_matches_prefix(projection, current,
                             pair.current_generation) ||
        !head_matches_prefix(
            projection, previous,
            pair.genesis
                ? 0U
                : pair.current_generation - 1U) ||
        !final_event_matches_delta(projection, &pair))
        return DCENT_RECEIPT_PROJECTION_MANIFEST_MISMATCH;

    memset(&parsed, 0, sizeof(parsed));
    parsed.generation = pair.current_generation;
    parsed.event_count = pair.current_generation;
    parsed.resource_count = current->resource_count;
    parsed.claim_present = current->claim_present;
    parsed.authority = current->authority;
    parsed.transaction_phase = current->transaction_phase;
    memcpy(parsed.current_head_sha256, current->record_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    parsed.initialized = true;
    *out = parsed;
    return DCENT_RECEIPT_PROJECTION_OK;
}

const char *dcent_receipt_projection_result_name(
    enum dcent_receipt_projection_result result)
{
    switch (result) {
    case DCENT_RECEIPT_PROJECTION_OK:
        return "ok";
    case DCENT_RECEIPT_PROJECTION_INVALID_ARGUMENT:
        return "invalid-argument";
    case DCENT_RECEIPT_PROJECTION_FORMAT:
        return "format";
    case DCENT_RECEIPT_PROJECTION_STORAGE:
        return "storage";
    case DCENT_RECEIPT_PROJECTION_LIMIT:
        return "limit";
    case DCENT_RECEIPT_PROJECTION_DUPLICATE_GENERATION:
        return "duplicate-generation";
    case DCENT_RECEIPT_PROJECTION_GENERATION_GAP:
        return "generation-gap";
    case DCENT_RECEIPT_PROJECTION_CHRONOLOGY:
        return "chronology";
    case DCENT_RECEIPT_PROJECTION_MANIFEST_MISMATCH:
        return "manifest-mismatch";
    }
    return "unknown";
}
