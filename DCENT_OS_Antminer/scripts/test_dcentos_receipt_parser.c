/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_format.h"

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define ZERO_SHA                                                               \
    "0000000000000000000000000000000000000000000000000000000000000000"
#define TX_ID "Tx-1"
#define CLAIM_ID "Claim-1"

static unsigned int assertions;
static const char *fixture_binding_sha256 = ZERO_SHA;

static void require(bool condition, const char *label)
{
    ++assertions;
    if (!condition) {
        fprintf(stderr, "FAIL: %s\n", label);
        exit(1);
    }
}

static void bytes_to_hex(const unsigned char *bytes, char hex[65])
{
    static const char digits[] = "0123456789abcdef";
    size_t index;

    for (index = 0; index < 32U; ++index) {
        hex[index * 2U] = digits[bytes[index] >> 4];
        hex[index * 2U + 1U] = digits[bytes[index] & 0x0fU];
    }
    hex[64] = '\0';
}

static void body_digest(const char *body, char hex[65])
{
    unsigned char digest[32];

    dcent_receipt_sha256(digest, body, strlen(body));
    bytes_to_hex(digest, hex);
}

static size_t checked_snprintf(char *buffer, size_t capacity,
                               const char *format, ...)
{
    va_list arguments;
    int length;

    va_start(arguments, format);
    length = vsnprintf(buffer, capacity, format, arguments);
    va_end(arguments);
    require(length >= 0 && (size_t)length < capacity, "fixture fits buffer");
    return (size_t)length;
}

static size_t build_binding(char *buffer, size_t capacity, const char *tx,
                            const char *boot, const char *pid,
                            const char *start, const char *mntns,
                            const char *lock_path, const char *lock_devino,
                            const char *ledger_path)
{
    return checked_snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-resource-ledger-abi1\n"
        "transaction_id=%s\n"
        "boot_id=%s\n"
        "owner_pid=%s\n"
        "owner_starttime=%s\n"
        "owner_mount_namespace=%s\n"
        "acquisition_guard_device_inode=9:10\n"
        "transaction_lock_path=%s\n"
        "transaction_lock_device_inode=%s\n"
        "transaction_lock_owner_device_inode=2:34\n"
        "transaction_lock_owner_sha256=" ZERO_SHA "\n"
        "storage_mount_id=7\n"
        "ledger_path=%s\n"
        "ledger_device_inode=5:99\n"
        "owner=zynq-sysupgrade\n",
        tx, boot, pid, start, mntns, lock_path, lock_devino, ledger_path);
}

static size_t build_lock_owner(char *buffer, size_t capacity, const char *tx,
                               const char *boot, const char *pid,
                               const char *start)
{
    return checked_snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-lock-v3\n"
        "transaction_id=%s\n"
        "boot_id=%s\n"
        "pid=%s\n"
        "starttime=%s\n"
        "owner=zynq-sysupgrade\n",
        tx, boot, pid, start);
}

static size_t build_resource_intent(
    char *buffer, size_t capacity, const char *kind, const char *id,
    const char *provenance, const char *a, const char *b, const char *c,
    const char *evidence_type, const char *body)
{
    char digest[65];
    size_t body_size = strlen(body);
    int header;

    body_digest(body, digest);
    header = snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-resource-intent-abi1\n"
        "binding_sha256=%s\n"
        "transaction_id=%s\n"
        "kind=%s\n"
        "resource_id=%s\n"
        "provenance=%s\n"
        "identity_a=%s\n"
        "identity_b=%s\n"
        "identity_c=%s\n"
        "evidence_type=%s\n"
        "evidence_size=%zu\n"
        "evidence_sha256=%s\n\n",
        fixture_binding_sha256, TX_ID, kind, id, provenance, a, b, c,
        evidence_type,
        body_size, digest);
    require(header >= 0 && (size_t)header + body_size < capacity,
            "resource-intent fixture fits buffer");
    memcpy(buffer + header, body, body_size);
    return (size_t)header + body_size;
}

static size_t build_resource_status_generation(
    char *buffer, size_t capacity, const char *kind, const char *id,
    const char *intent_sha, const char *phase, unsigned int revision,
    unsigned int generation, const char *previous, const char *actor_kind,
    const char *actor_id, const char *evidence_type, const char *body)
{
    char digest[65];
    const char *digest_text = "-";
    size_t body_size = strlen(body);
    int header;

    if (body_size != 0U) {
        body_digest(body, digest);
        digest_text = digest;
    }
    header = snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-resource-status-abi1\n"
        "binding_sha256=%s\n"
        "transaction_id=%s\n"
        "kind=%s\n"
        "resource_id=%s\n"
        "intent_sha256=%s\n"
        "phase=%s\n"
        "revision=%u\n"
        "ledger_generation=%u\n"
        "previous_status_sha256=%s\n"
        "actor_kind=%s\n"
        "actor_id=%s\n"
        "evidence_type=%s\n"
        "evidence_size=%zu\n"
        "evidence_sha256=%s\n\n",
        fixture_binding_sha256, TX_ID, kind, id, intent_sha, phase, revision,
        generation, previous, actor_kind, actor_id, evidence_type, body_size,
        digest_text);
    require(header >= 0 && (size_t)header + body_size < capacity,
            "resource-status fixture fits buffer");
    memcpy(buffer + header, body, body_size);
    return (size_t)header + body_size;
}

static size_t build_resource_status(
    char *buffer, size_t capacity, const char *kind, const char *id,
    const char *intent_sha, const char *phase, unsigned int revision,
    const char *previous, const char *actor_kind, const char *actor_id,
    const char *evidence_type, const char *body)
{
    return build_resource_status_generation(
        buffer, capacity, kind, id, intent_sha, phase, revision, revision,
        previous, actor_kind, actor_id, evidence_type, body);
}

static size_t build_claim_intent(char *buffer, size_t capacity,
                                 const char *body)
{
    char digest[65];
    size_t body_size = strlen(body);
    int header;

    body_digest(body, digest);
    header = snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-reconcile-intent-abi1\n"
        "binding_sha256=%s\n"
        "transaction_id=%s\n"
        "claim_id=%s\n"
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
        fixture_binding_sha256, TX_ID, CLAIM_ID, body_size, digest);
    require(header >= 0 && (size_t)header + body_size < capacity,
            "claim-intent fixture fits buffer");
    memcpy(buffer + header, body, body_size);
    return (size_t)header + body_size;
}

static size_t build_claim_status_generation(
    char *buffer, size_t capacity, const char *intent_sha, const char *phase,
    unsigned int revision, unsigned int generation, const char *previous,
    const char *quiescence, const char *evidence_type, const char *body)
{
    char digest[65];
    const char *digest_text = "-";
    size_t body_size = strlen(body);
    int header;

    if (body_size != 0U) {
        body_digest(body, digest);
        digest_text = digest;
    }
    header = snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-reconcile-status-abi1\n"
        "claim_intent_sha256=%s\n"
        "phase=%s\n"
        "revision=%u\n"
        "ledger_generation=%u\n"
        "previous_status_sha256=%s\n"
        "actor_id=%s\n"
        "quiescence_sha256=%s\n"
        "evidence_type=%s\n"
        "evidence_size=%zu\n"
        "evidence_sha256=%s\n\n",
        intent_sha, phase, revision, generation, previous, CLAIM_ID, quiescence,
        evidence_type, body_size, digest_text);
    require(header >= 0 && (size_t)header + body_size < capacity,
            "claim-status fixture fits buffer");
    memcpy(buffer + header, body, body_size);
    return (size_t)header + body_size;
}

static size_t build_claim_status(
    char *buffer, size_t capacity, const char *intent_sha, const char *phase,
    unsigned int revision, const char *previous, const char *quiescence,
    const char *evidence_type, const char *body)
{
    return build_claim_status_generation(
        buffer, capacity, intent_sha, phase, revision, revision, previous,
        quiescence, evidence_type, body);
}

static size_t build_phase_status(
    char *buffer, size_t capacity, const char *phase, unsigned int revision,
    unsigned int generation, const char *previous, const char *actor_kind,
    const char *actor_id, const char *evidence_type, const char *body)
{
    char digest[65];
    size_t body_size = strlen(body);
    int header;

    body_digest(body, digest);
    header = snprintf(
        buffer, capacity,
        "schema=dcentos-sysupgrade-transaction-phase-status-abi2\n"
        "binding_sha256=%s\n"
        "transaction_id=%s\n"
        "phase=%s\n"
        "revision=%u\n"
        "ledger_generation=%u\n"
        "previous_status_sha256=%s\n"
        "actor_kind=%s\n"
        "actor_id=%s\n"
        "evidence_type=%s\n"
        "evidence_size=%zu\n"
        "evidence_sha256=%s\n\n",
        fixture_binding_sha256, TX_ID, phase, revision, generation, previous,
        actor_kind, actor_id, evidence_type, body_size, digest);
    require(header >= 0 && (size_t)header + body_size < capacity,
            "transaction-phase fixture fits buffer");
    memcpy(buffer + header, body, body_size);
    return (size_t)header + body_size;
}

static void replace_byte(char *buffer, size_t size, const char *needle,
                         char replacement)
{
    char *found = strstr(buffer, needle);

    require(found != NULL && (size_t)(found - buffer) < size,
            "mutation needle exists");
    *found = replacement;
}

static void test_registry(void)
{
    static const char *const names[] = {
        "attachment-intent-v1", "attachment-active-v1",
        "attachment-release-pending-v1", "attachment-released-v1",
        "attachment-conflict-v1", "node-intent-v1", "node-active-v1",
        "node-release-pending-v1", "node-released-v1", "node-conflict-v1",
        "mount-intent-v1", "mount-active-v1", "mount-release-pending-v1",
        "mount-released-v1", "mount-conflict-v1", "workspace-intent-v1",
        "workspace-active-v1", "workspace-release-pending-v1",
        "workspace-released-v1", "workspace-conflict-v1", "owner-death-v1",
        "maintenance-quiescence-v1", "reconciliation-begin-v1",
        "reconciliation-complete-v1", "reconciliation-blocked-v1",
        "transaction-cleanup-required-v1",
        "transaction-env-commit-armed-v1",
        "transaction-env-commit-disarmed-v1",
        "transaction-env-committed-v1",
    };
    size_t index;

    for (index = 0; index < sizeof(names) / sizeof(names[0]); ++index) {
        enum dcent_receipt_evidence_type type =
            dcent_receipt_evidence_type_parse(names[index]);

        require(type != DCENT_RECEIPT_EVIDENCE_INVALID,
                "registered evidence type parses");
        require(strcmp(dcent_receipt_evidence_type_name(type), names[index]) ==
                    0,
                "registered evidence type round trips");
    }
    require(dcent_receipt_evidence_type_parse("future-v2") ==
                DCENT_RECEIPT_EVIDENCE_INVALID,
            "unknown evidence type refused");
    require(dcent_receipt_resource_kind_parse("attachment") ==
                DCENT_RECEIPT_KIND_ATTACHMENT,
            "resource kind parses");
    require(dcent_receipt_actor_kind_parse("reconciler") ==
                DCENT_RECEIPT_ACTOR_RECONCILER,
            "actor kind parses");
}

static void test_binding(void)
{
    char fixture[4096];
    char mutation[4096];
    char oversized[4097];
    struct dcent_receipt_binding parsed;
    struct dcent_receipt_binding sentinel;
    size_t size;
    size_t index;

    size = build_binding(
        fixture, sizeof(fixture), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "123", "456", "1:22",
        "/run/dcentos-sysupgrade.lock", "2:33",
        "/run/dcentos-sysupgrade.lock/ledger");
    require(dcent_receipt_parse_binding_abi1(fixture, size, &parsed) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical binding parses");
    require(parsed.owner_pid == 123U && parsed.owner_starttime == 456U,
            "binding numbers decode");
    require(parsed.owner_mount_namespace.device == 1U &&
                parsed.owner_mount_namespace.inode == 22U,
            "binding namespace decodes");
    require(parsed.acquisition_guard_device_inode.device == 9U &&
                parsed.acquisition_guard_device_inode.inode == 10U &&
                parsed.transaction_lock_owner_device_inode.device == 2U &&
                parsed.transaction_lock_owner_device_inode.inode == 34U &&
                parsed.transaction_lock_owner_sha256.present &&
                parsed.storage_mount_id == 7U,
            "binding owns guard, immutable owner, and mount identity");
    require(parsed.ledger_device_inode.device == 5U &&
                parsed.ledger_device_inode.inode == 99U,
            "binding authenticates the ledger directory identity");

    for (index = 0; index < size; ++index) {
        require(dcent_receipt_parse_binding_abi1(fixture, index, &parsed) !=
                    DCENT_RECEIPT_FORMAT_OK,
                "binding truncation refused at every byte");
    }
    memcpy(mutation, fixture, size);
    mutation[size] = 'x';
    require(dcent_receipt_parse_binding_abi1(mutation, size + 1U, &parsed) !=
                DCENT_RECEIPT_FORMAT_OK,
            "binding trailing byte refused");
    memcpy(mutation, fixture, size + 1U);
    replace_byte(mutation, size, "abi1", 'A');
    require(dcent_receipt_parse_binding_abi1(mutation, size, &parsed) !=
                DCENT_RECEIPT_FORMAT_OK,
            "wrong schema refused");
    memcpy(mutation, fixture, size + 1U);
    replace_byte(mutation, size, "abcdef12", 'A');
    require(dcent_receipt_parse_binding_abi1(mutation, size, &parsed) !=
                DCENT_RECEIPT_FORMAT_OK,
            "uppercase UUID refused");
    memcpy(mutation, fixture, size + 1U);
    mutation[10] = '\r';
    require(dcent_receipt_parse_binding_abi1(mutation, size, &parsed) !=
                DCENT_RECEIPT_FORMAT_OK,
            "CR refused");
    memcpy(mutation, fixture, size + 1U);
    mutation[10] = '\t';
    require(dcent_receipt_parse_binding_abi1(mutation, size, &parsed) !=
                DCENT_RECEIPT_FORMAT_OK,
            "tab refused");
    memcpy(mutation, fixture, size + 1U);
    mutation[10] = '\0';
    require(dcent_receipt_parse_binding_abi1(mutation, size, &parsed) !=
                DCENT_RECEIPT_FORMAT_OK,
            "NUL refused");
    memcpy(mutation, fixture, size + 1U);
    mutation[10] = (char)0x80;
    require(dcent_receipt_parse_binding_abi1(mutation, size, &parsed) !=
                DCENT_RECEIPT_FORMAT_OK,
            "non-ASCII refused");
    memcpy(mutation, fixture, size + 1U);
    {
        char *mount_id = strstr(mutation, "storage_mount_id=7");

        require(mount_id != NULL, "mount-id mutation field exists");
        mount_id[strlen("storage_mount_id=")] = '0';
    }
    require(dcent_receipt_parse_binding_abi1(mutation, size, &parsed) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "zero storage mount ID refused");

    size = build_binding(
        fixture, sizeof(fixture), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "0", "456", "1:22",
        "/run/dcentos-sysupgrade.lock", "2:33",
        "/run/dcentos-sysupgrade.lock/ledger");
    memset(&sentinel, 0xa5, sizeof(sentinel));
    parsed = sentinel;
    require(dcent_receipt_parse_binding_abi1(fixture, size, &parsed) !=
                DCENT_RECEIPT_FORMAT_OK,
            "zero PID refused");
    require(memcmp(&parsed, &sentinel, sizeof(parsed)) == 0,
            "failed parser does not partially populate output");
    size = build_binding(
        fixture, sizeof(fixture), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "2147483648", "456",
        "1:22", "/run/dcentos-sysupgrade.lock", "2:33",
        "/run/dcentos-sysupgrade.lock/ledger");
    require(dcent_receipt_parse_binding_abi1(fixture, size, &parsed) ==
                DCENT_RECEIPT_FORMAT_LIMIT,
            "PID overflow refused as a limit");
    size = build_binding(
        fixture, sizeof(fixture), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "123", "0456", "1:22",
        "/run/dcentos-sysupgrade.lock", "2:33",
        "/run/dcentos-sysupgrade.lock/ledger");
    require(dcent_receipt_parse_binding_abi1(fixture, size, &parsed) !=
                DCENT_RECEIPT_FORMAT_OK,
            "leading-zero start time refused");
    size = build_binding(
        fixture, sizeof(fixture), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "123", "456", "1:2:3",
        "/run/dcentos-sysupgrade.lock", "2:33",
        "/run/dcentos-sysupgrade.lock/ledger");
    require(dcent_receipt_parse_binding_abi1(fixture, size, &parsed) !=
                DCENT_RECEIPT_FORMAT_OK,
            "multi-colon device identity refused");
    size = build_binding(
        fixture, sizeof(fixture), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "123", "456", "1:22",
        "/run/dcentos-sysupgrade.lock/", "2:33",
        "/run/dcentos-sysupgrade.lock/ledger");
    require(dcent_receipt_parse_binding_abi1(fixture, size, &parsed) !=
                DCENT_RECEIPT_FORMAT_OK,
            "trailing-slash path refused");
    memset(oversized, 'x', sizeof(oversized));
    require(dcent_receipt_parse_binding_abi1(oversized, sizeof(oversized),
                                             &parsed) ==
                DCENT_RECEIPT_FORMAT_LIMIT,
            "receipt above 4096 bytes refused before parsing");
    memset(oversized, 'x', 641U);
    oversized[640] = '\n';
    require(dcent_receipt_parse_binding_abi1(oversized, 641U, &parsed) ==
                DCENT_RECEIPT_FORMAT_LIMIT,
            "header line above 640 bytes refused");
}

static void test_lock_owner(void)
{
    char fixture[4096];
    char mutation[4096];
    struct dcent_receipt_lock_owner owner;
    struct dcent_receipt_lock_owner sentinel;
    struct dcent_receipt_lock_anchor anchor;
    size_t size;
    size_t index;

    size = build_lock_owner(
        fixture, sizeof(fixture), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "123", "456");
    require(dcent_receipt_parse_lock_owner_v3(fixture, size, &owner) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical immutable lock-v3 owner parses");
    require(owner.pid == 123U && owner.starttime == 456U &&
                owner.transaction_id.size == strlen(TX_ID),
            "lock-v3 owner fields decode");
    require(dcent_receipt_lock_anchor_init(&anchor, &owner) ==
                DCENT_RECEIPT_FORMAT_OK,
            "lock-v3 owner converts to an owned authority anchor");
    memset(fixture, 0xa5, size);
    require(anchor.transaction_id.size == strlen(TX_ID) &&
                anchor.boot_id.size == 36U && anchor.pid == 123U &&
                anchor.starttime == 456U,
            "lock-v3 authority anchor survives parser-buffer reuse");

    size = build_lock_owner(
        fixture, sizeof(fixture), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "123", "456");
    for (index = 0; index < size; ++index) {
        require(dcent_receipt_parse_lock_owner_v3(fixture, index, &owner) !=
                    DCENT_RECEIPT_FORMAT_OK,
                "lock-v3 truncation refused at every byte");
    }
    size = build_lock_owner(
        fixture, sizeof(fixture), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "0", "456");
    memset(&sentinel, 0xa5, sizeof(sentinel));
    owner = sentinel;
    require(dcent_receipt_parse_lock_owner_v3(fixture, size, &owner) !=
                DCENT_RECEIPT_FORMAT_OK,
            "zero lock owner PID refused");
    require(memcmp(&owner, &sentinel, sizeof(owner)) == 0,
            "failed lock parser leaves output untouched");
    size = build_lock_owner(
        fixture, sizeof(fixture), "bad:id",
        "abcdef12-3456-7890-abcd-ef1234567890", "123", "456");
    require(dcent_receipt_parse_lock_owner_v3(fixture, size, &owner) !=
                DCENT_RECEIPT_FORMAT_OK,
            "invalid lock transaction ID refused");
    size = build_lock_owner(
        fixture, sizeof(fixture), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "123", "456");
    memcpy(mutation, fixture, size);
    replace_byte(mutation, size, "lock-v3", 'L');
    require(dcent_receipt_parse_lock_owner_v3(mutation, size, &owner) !=
                DCENT_RECEIPT_FORMAT_OK,
            "foreign lock schema refused");
    memcpy(mutation, fixture, size);
    mutation[size] = 'x';
    require(dcent_receipt_parse_lock_owner_v3(mutation, size + 1U, &owner) !=
                DCENT_RECEIPT_FORMAT_OK,
            "lock-v3 trailing byte refused");
}

static void test_transaction_phase_records(void)
{
    char binding_buffer[4096];
    char fixture[4096];
    char previous[65];
    char binding_hex[65];
    struct dcent_receipt_binding binding;
    struct dcent_receipt_binding_anchor anchor;
    struct dcent_receipt_transaction_phase_status status;
    struct dcent_receipt_transaction_phase_status sentinel;
    struct dcent_receipt_transaction_phase_chain chain;
    struct dcent_receipt_transaction_phase_chain snapshot;
    size_t size;
    size_t index;
    unsigned int revision;

    size = build_phase_status(
        fixture, sizeof(fixture), "env-commit-armed", 1U, 1U, "-", "owner",
        TX_ID, "transaction-env-commit-armed-v1",
        "environment_digest=verified\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_OK,
            "canonical transaction-phase status parses");
    require(status.phase == DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED &&
                status.revision == 1U && status.ledger_generation == 1U &&
                status.actor_kind == DCENT_RECEIPT_ACTOR_OWNER,
            "transaction-phase status fields decode");
    for (index = 0U; index < size; ++index) {
        require(dcent_receipt_parse_transaction_phase_status_abi2(
                    fixture, index, &status) != DCENT_RECEIPT_FORMAT_OK,
                "transaction-phase truncation refused at every byte");
    }
    memset(&sentinel, 0xa5, sizeof(sentinel));
    status = sentinel;
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, 0U, &status) != DCENT_RECEIPT_FORMAT_OK,
            "empty transaction-phase status refused");
    require(memcmp(&status, &sentinel, sizeof(status)) == 0,
            "failed transaction-phase parser leaves output untouched");

    size = build_phase_status(
        fixture, sizeof(fixture), "env-commit-armed", 1U, 0U, "-", "owner",
        TX_ID, "transaction-env-commit-armed-v1",
        "environment_digest=verified\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_SEMANTIC,
            "transaction-phase generation zero refused");
    size = build_phase_status(
        fixture, sizeof(fixture), "env-commit-armed", 1U, 149U, "-", "owner",
        TX_ID, "transaction-env-commit-armed-v1",
        "environment_digest=verified\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_LIMIT,
            "transaction-phase generation above 148 refused");
    size = build_phase_status(
        fixture, sizeof(fixture), "env-commit-armed", 1U, 1U, "-",
        "reconciler", CLAIM_ID, "transaction-env-commit-armed-v1",
        "environment_digest=verified\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_SEMANTIC,
            "reconciler cannot arm environment commit");
    size = build_phase_status(
        fixture, sizeof(fixture), "active", 1U, 1U, "-", "owner", TX_ID,
        "transaction-env-commit-armed-v1",
        "environment_digest=verified\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_SEMANTIC,
            "transaction phase requires transition-specific evidence");

    size = build_binding(
        binding_buffer, sizeof(binding_buffer), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "123", "456", "1:22",
        "/run/dcentos-sysupgrade.lock", "2:33",
        "/run/dcentos-sysupgrade.lock/ledger");
    require(dcent_receipt_parse_binding_abi1(binding_buffer, size, &binding) ==
                DCENT_RECEIPT_FORMAT_OK &&
                dcent_receipt_binding_anchor_init(&anchor, &binding) ==
                    DCENT_RECEIPT_FORMAT_OK,
            "transaction-phase binding anchor initializes");
    bytes_to_hex(binding.record_sha256, binding_hex);
    fixture_binding_sha256 = binding_hex;
    require(dcent_receipt_transaction_phase_chain_begin(&chain, &anchor) ==
                DCENT_RECEIPT_FORMAT_OK &&
                dcent_receipt_transaction_phase_chain_finish(&chain) ==
                    DCENT_RECEIPT_FORMAT_OK,
            "implicit active genesis phase is a complete chain");

    size = build_phase_status(
        fixture, sizeof(fixture), "env-commit-armed", 1U, 5U, "-", "owner",
        TX_ID, "transaction-env-commit-armed-v1",
        "environment_digest=verified\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_OK &&
                dcent_receipt_transaction_phase_chain_add(&chain, &status) ==
                    DCENT_RECEIPT_FORMAT_OK,
            "active transaction arms environment commit");
    bytes_to_hex(status.record_sha256, previous);
    snapshot = chain;
    size = build_phase_status(
        fixture, sizeof(fixture), "active", 2U, 5U, previous, "owner", TX_ID,
        "transaction-env-commit-disarmed-v1", "disarm_verified=true\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_OK &&
                dcent_receipt_transaction_phase_chain_add(&chain, &status) ==
                    DCENT_RECEIPT_FORMAT_SEMANTIC,
            "transaction-phase generations increase strictly");
    require(memcmp(&chain, &snapshot, sizeof(chain)) == 0,
            "failed phase-chain add leaves state untouched");
    size = build_phase_status(
        fixture, sizeof(fixture), "active", 2U, 6U, previous, "owner", TX_ID,
        "transaction-env-commit-disarmed-v1", "disarm_verified=true\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_OK &&
                dcent_receipt_transaction_phase_chain_add(&chain, &status) ==
                    DCENT_RECEIPT_FORMAT_OK,
            "authenticated evidence disarms an unmodified environment");
    bytes_to_hex(status.record_sha256, previous);
    size = build_phase_status(
        fixture, sizeof(fixture), "env-commit-armed", 3U, 7U, previous,
        "owner", TX_ID, "transaction-env-commit-armed-v1",
        "environment_digest=verified\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_OK &&
                dcent_receipt_transaction_phase_chain_add(&chain, &status) ==
                    DCENT_RECEIPT_FORMAT_OK,
            "transaction may re-arm after proven disarm");
    bytes_to_hex(status.record_sha256, previous);
    size = build_phase_status(
        fixture, sizeof(fixture), "env-committed", 4U, 8U, previous, "owner",
        TX_ID, "transaction-env-committed-v1", "commit_verified=true\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_OK &&
                dcent_receipt_transaction_phase_chain_add(&chain, &status) ==
                    DCENT_RECEIPT_FORMAT_OK &&
                dcent_receipt_transaction_phase_chain_finish(&chain) ==
                    DCENT_RECEIPT_FORMAT_OK,
            "armed transaction reaches terminal committed phase");
    bytes_to_hex(status.record_sha256, previous);
    snapshot = chain;
    size = build_phase_status(
        fixture, sizeof(fixture), "cleanup-required", 5U, 9U, previous,
        "owner", TX_ID, "transaction-cleanup-required-v1",
        "cleanup_required=true\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_OK &&
                dcent_receipt_transaction_phase_chain_add(&chain, &status) ==
                    DCENT_RECEIPT_FORMAT_SEMANTIC,
            "committed phase is terminal");
    require(memcmp(&chain, &snapshot, sizeof(chain)) == 0,
            "terminal phase rejection preserves the chain");

    require(dcent_receipt_transaction_phase_chain_begin(&chain, &anchor) ==
                DCENT_RECEIPT_FORMAT_OK,
            "cleanup phase branch begins from genesis");
    size = build_phase_status(
        fixture, sizeof(fixture), "cleanup-required", 1U, 10U, "-",
        "reconciler", CLAIM_ID, "transaction-cleanup-required-v1",
        "cleanup_required=true\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_OK &&
                dcent_receipt_transaction_phase_chain_add(&chain, &status) ==
                    DCENT_RECEIPT_FORMAT_OK,
            "reconciler may commit terminal cleanup-required phase");

    require(dcent_receipt_transaction_phase_chain_begin(&chain, &anchor) ==
                DCENT_RECEIPT_FORMAT_OK,
            "bounded phase retry chain begins");
    strcpy(previous, "-");
    for (revision = 1U; revision <= DCENT_RECEIPT_MAX_PHASE_REVISIONS;
         ++revision) {
        const bool armed = (revision & 1U) != 0U;

        size = build_phase_status(
            fixture, sizeof(fixture), armed ? "env-commit-armed" : "active",
            revision, 100U + revision, previous, "owner", TX_ID,
            armed ? "transaction-env-commit-armed-v1"
                  : "transaction-env-commit-disarmed-v1",
            armed ? "environment_digest=verified\n"
                  : "disarm_verified=true\n");
        require(dcent_receipt_parse_transaction_phase_status_abi2(
                    fixture, size, &status) == DCENT_RECEIPT_FORMAT_OK &&
                    dcent_receipt_transaction_phase_chain_add(
                        &chain, &status) == DCENT_RECEIPT_FORMAT_OK,
                "bounded phase chain admits every revision through sixteen");
        bytes_to_hex(status.record_sha256, previous);
    }
    require(dcent_receipt_transaction_phase_chain_finish(&chain) ==
                DCENT_RECEIPT_FORMAT_OK &&
                chain.revisions == DCENT_RECEIPT_MAX_PHASE_REVISIONS,
            "transaction-phase revision sixteen is retained");
    size = build_phase_status(
        fixture, sizeof(fixture), "env-commit-armed", 17U, 117U, previous,
        "owner", TX_ID, "transaction-env-commit-armed-v1",
        "environment_digest=verified\n");
    require(dcent_receipt_parse_transaction_phase_status_abi2(
                fixture, size, &status) == DCENT_RECEIPT_FORMAT_LIMIT,
            "transaction-phase revision seventeen is refused");
    fixture_binding_sha256 = ZERO_SHA;
}

static void test_resource_records(void)
{
    static const struct {
        const char *kind;
        const char *id;
        const char *a;
        const char *b;
        const char *c;
        const char *type;
        enum dcent_receipt_resource_kind expected;
    } intents[] = {
        {"attachment", "ubi-1", "7", "1", "-", "attachment-intent-v1",
         DCENT_RECEIPT_KIND_ATTACHMENT},
        {"node", "ubi1-0", "/dev/ubi1_0", "5:99", "-", "node-intent-v1",
         DCENT_RECEIPT_KIND_NODE},
        {"mount", "inactive-data", "/dev/ubi1_2", "/tmp/inactive-data", "rw",
         "mount-intent-v1", DCENT_RECEIPT_KIND_MOUNT},
        {"workspace", "work", "/tmp/dcent-work", "6:100", "-",
         "workspace-intent-v1", DCENT_RECEIPT_KIND_WORKSPACE},
    };
    char fixture[4096];
    char mutation[4096];
    char max_body[1026];
    char line_body[512];
    char path[514];
    char long_id[66];
    struct dcent_receipt_resource_intent intent;
    struct dcent_receipt_resource_intent intent_sentinel;
    struct dcent_receipt_resource_status status;
    struct dcent_receipt_resource_status status_sentinel;
    size_t size;
    size_t index;

    for (index = 0; index < sizeof(intents) / sizeof(intents[0]); ++index) {
        size = build_resource_intent(
            fixture, sizeof(fixture), intents[index].kind, intents[index].id,
            "created", intents[index].a, intents[index].b, intents[index].c,
            intents[index].type, "observed=true\n");
        require(dcent_receipt_parse_resource_intent_abi1(fixture, size,
                                                         &intent) ==
                    DCENT_RECEIPT_FORMAT_OK,
                "canonical resource intent parses for every kind");
        require(intent.kind == intents[index].expected,
                "resource kind decodes exactly");
        require(intent.evidence.body.size == strlen("observed=true\n"),
                "intent embeds exact evidence bytes");
    }

    memset(long_id, 'a', 64U);
    long_id[64] = '\0';
    size = build_resource_intent(
        fixture, sizeof(fixture), "attachment", long_id, "borrowed", "7", "1",
        "-", "attachment-intent-v1", "observed=true\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "64-byte ID accepted");
    long_id[64] = 'a';
    long_id[65] = '\0';
    size = build_resource_intent(
        fixture, sizeof(fixture), "attachment", long_id, "borrowed", "7", "1",
        "-", "attachment-intent-v1", "observed=true\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) !=
                DCENT_RECEIPT_FORMAT_OK,
            "65-byte ID refused");
    size = build_resource_intent(
        fixture, sizeof(fixture), "attachment", ".hidden", "created", "7", "1",
        "-", "attachment-intent-v1", "observed=true\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) !=
                DCENT_RECEIPT_FORMAT_OK,
            "leading-dot ID refused");
    size = build_resource_intent(
        fixture, sizeof(fixture), "attachment", "ubi-1", "created", "07", "1",
        "-", "attachment-intent-v1", "observed=true\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) !=
                DCENT_RECEIPT_FORMAT_OK,
            "noncanonical attachment number refused");
    size = build_resource_intent(
        fixture, sizeof(fixture), "mount", "mnt", "created", "/dev/ubi1_2",
        "/tmp/../escape", "rw", "mount-intent-v1", "observed=true\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) !=
                DCENT_RECEIPT_FORMAT_OK,
            "dotdot path refused");
    size = build_resource_intent(
        fixture, sizeof(fixture), "node", "node", "created", "/dev/ubi1_0", "1:2",
        "-", "mount-intent-v1", "observed=true\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "kind-mismatched intent evidence refused");
    size = build_resource_intent(
        fixture, sizeof(fixture), "node", "node", "created", "/dev/ubi1_0", "1:2",
        "-", "future-v2", "observed=true\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "unknown evidence type refused");
    size = build_resource_intent(
        fixture, sizeof(fixture), "node", "node", "created", "/dev/ubi1_0", "1:2",
        "-", "node-intent-v1", "a=1\na=2\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "duplicate evidence key refused");
    size = build_resource_intent(
        fixture, sizeof(fixture), "node", "node", "created", "/dev/ubi1_0", "1:2",
        "-", "node-intent-v1", "a=hello world\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) !=
                DCENT_RECEIPT_FORMAT_OK,
            "evidence value space refused");
    size = build_resource_intent(
        fixture, sizeof(fixture), "node", "node", "created", "/dev/ubi1_0", "1:2",
        "-", "node-intent-v1", "a=1\n\nb=2\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) !=
                DCENT_RECEIPT_FORMAT_OK,
            "blank evidence line refused");

    {
        size_t offset = 0;

        for (index = 0; index < 32U; ++index) {
            int written = snprintf(line_body + offset,
                                   sizeof(line_body) - offset, "k%02zu=v\n",
                                   index);
            require(written > 0 && (size_t)written < sizeof(line_body) - offset,
                    "evidence-line fixture fits");
            offset += (size_t)written;
        }
        size = build_resource_intent(
            fixture, sizeof(fixture), "node", "node", "created",
            "/dev/ubi1_0", "1:2", "-", "node-intent-v1", line_body);
        require(dcent_receipt_parse_resource_intent_abi1(fixture, size,
                                                         &intent) ==
                    DCENT_RECEIPT_FORMAT_OK,
                "32-line evidence accepted");
        require((size_t)snprintf(line_body + offset,
                                 sizeof(line_body) - offset, "k32=v\n") <
                    sizeof(line_body) - offset,
                "33rd evidence line fixture fits");
        size = build_resource_intent(
            fixture, sizeof(fixture), "node", "node", "created",
            "/dev/ubi1_0", "1:2", "-", "node-intent-v1", line_body);
        require(dcent_receipt_parse_resource_intent_abi1(fixture, size,
                                                         &intent) ==
                    DCENT_RECEIPT_FORMAT_LIMIT,
                "33rd evidence line refused as a limit");
    }

    path[0] = '/';
    memset(path + 1, 'p', 511U);
    path[512] = '\0';
    size = build_resource_intent(
        fixture, sizeof(fixture), "node", "node", "created", path, "1:2", "-",
        "node-intent-v1", "observed=true\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "512-byte path accepted");
    path[512] = 'p';
    path[513] = '\0';
    size = build_resource_intent(
        fixture, sizeof(fixture), "node", "node", "created", path, "1:2", "-",
        "node-intent-v1", "observed=true\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) !=
                DCENT_RECEIPT_FORMAT_OK,
            "513-byte path refused");
    size = build_resource_intent(
        fixture, sizeof(fixture), "node", "node", "created", "/dev/ubi1_0",
        "1:18446744073709551616", "-", "node-intent-v1",
        "observed=true\n");
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "overflowing inode refused");

    max_body[0] = 'k';
    max_body[1] = '=';
    memset(max_body + 2, 'x', 1021U);
    max_body[1023] = '\n';
    max_body[1024] = '\0';
    size = build_resource_intent(
        fixture, sizeof(fixture), "node", "node", "created", "/dev/ubi1_0", "1:2",
        "-", "node-intent-v1", max_body);
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "1024-byte evidence accepted");
    max_body[1023] = 'x';
    max_body[1024] = '\n';
    max_body[1025] = '\0';
    size = build_resource_intent(
        fixture, sizeof(fixture), "node", "node", "created", "/dev/ubi1_0", "1:2",
        "-", "node-intent-v1", max_body);
    require(dcent_receipt_parse_resource_intent_abi1(fixture, size, &intent) ==
                DCENT_RECEIPT_FORMAT_LIMIT,
            "1025-byte evidence refused as a limit");

    size = build_resource_intent(
        fixture, sizeof(fixture), "node", "node", "created", "/dev/ubi1_0", "1:2",
        "-", "node-intent-v1", "observed=true\n");
    for (index = 0; index < size; ++index) {
        require(dcent_receipt_parse_resource_intent_abi1(fixture, index,
                                                         &intent) !=
                    DCENT_RECEIPT_FORMAT_OK,
                "resource intent truncation refused at every byte");
    }
    memcpy(mutation, fixture, size);
    replace_byte(mutation, size, "evidence_sha256=", 'E');
    require(dcent_receipt_parse_resource_intent_abi1(mutation, size, &intent) !=
                DCENT_RECEIPT_FORMAT_OK,
            "evidence field prefix confusion refused");
    memcpy(mutation, fixture, size);
    {
        char *digest = strstr(mutation, "evidence_sha256=");
        size_t offset = strlen("evidence_sha256=");
        require(digest != NULL, "evidence digest mutation exists");
        digest[offset] = digest[offset] == '0' ? '1' : '0';
    }
    require(dcent_receipt_parse_resource_intent_abi1(mutation, size, &intent) ==
                DCENT_RECEIPT_FORMAT_DIGEST_MISMATCH,
            "evidence digest mismatch refused distinctly");

    size = build_resource_status(fixture, sizeof(fixture), "attachment", "ubi-1",
                                 ZERO_SHA, "pending", 1, "-", "owner", TX_ID,
                                 "-", "");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical pending status parses");
    require(status.evidence.type == DCENT_RECEIPT_EVIDENCE_NONE &&
                status.ledger_generation == 1U,
            "pending status has generation and exact no-evidence sentinel");
    for (index = 0; index < size; ++index) {
        require(dcent_receipt_parse_resource_status_abi1(fixture, index,
                                                         &status) !=
                    DCENT_RECEIPT_FORMAT_OK,
                "resource status truncation refused at every byte");
    }
    size = build_resource_status_generation(
        fixture, sizeof(fixture), "attachment", "ubi-1", ZERO_SHA, "pending",
        1U, 0U, "-", "owner", TX_ID, "-", "");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "resource ledger generation zero refused");
    size = build_resource_status_generation(
        fixture, sizeof(fixture), "attachment", "ubi-1", ZERO_SHA, "pending",
        1U, 149U, "-", "owner", TX_ID, "-", "");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_LIMIT,
            "resource ledger generation above 148 refused");
    size = build_resource_status(
        fixture, sizeof(fixture), "attachment", "ubi-1", ZERO_SHA, "active", 2,
        ZERO_SHA, "owner", TX_ID, "attachment-active-v1", "attached=true\n");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical active status parses");
    size = build_resource_status(
        fixture, sizeof(fixture), "attachment", "ubi-1", ZERO_SHA,
        "release-pending", 3, ZERO_SHA, "owner", TX_ID,
        "attachment-release-pending-v1", "still_owned=true\n");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical release-pending status parses");
    size = build_resource_status(
        fixture, sizeof(fixture), "attachment", "ubi-1", ZERO_SHA, "released", 4,
        ZERO_SHA, "owner", TX_ID, "attachment-released-v1", "absent=true\n");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical released status parses");
    size = build_resource_status(
        fixture, sizeof(fixture), "attachment", "ubi-1", ZERO_SHA, "conflict", 2,
        ZERO_SHA, "reconciler", CLAIM_ID, "attachment-conflict-v1",
        "ambiguous=true\n");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical conflict status parses");
    size = build_resource_status(
        fixture, sizeof(fixture), "attachment", "ubi-1", ZERO_SHA, "active", 3,
        ZERO_SHA, "owner", TX_ID, "attachment-active-v1", "attached=true\n");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "phase/revision mismatch refused");
    size = build_resource_status(
        fixture, sizeof(fixture), "attachment", "ubi-1", ZERO_SHA, "conflict", 5,
        ZERO_SHA, "owner", TX_ID, "attachment-conflict-v1",
        "ambiguous=true\n");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_LIMIT,
            "fifth status revision refused as a limit");
    size = build_resource_status(
        fixture, sizeof(fixture), "attachment", "ubi-1", ZERO_SHA, "conflict", 9,
        ZERO_SHA, "owner", TX_ID, "attachment-conflict-v1",
        "ambiguous=true\n");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_LIMIT,
            "single digit above revision maximum refused without underflow");
    size = build_resource_status(
        fixture, sizeof(fixture), "attachment", "ubi-1", ZERO_SHA, "conflict",
        49, ZERO_SHA, "owner", TX_ID, "attachment-conflict-v1",
        "ambiguous=true\n");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_LIMIT,
            "multi-digit revision above maximum refused without underflow");
    size = build_resource_status(fixture, sizeof(fixture), "attachment", "ubi-1",
                                 ZERO_SHA, "pending", 1, "-", "owner", TX_ID,
                                 "attachment-active-v1", "attached=true\n");
    require(dcent_receipt_parse_resource_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "pending status with caller evidence refused");
    memset(&intent_sentinel, 0xa5, sizeof(intent_sentinel));
    intent = intent_sentinel;
    require(dcent_receipt_parse_resource_intent_abi1(fixture, 0, &intent) !=
                DCENT_RECEIPT_FORMAT_OK,
            "failed resource intent parse returns an error");
    require(memcmp(&intent, &intent_sentinel, sizeof(intent)) == 0,
            "failed resource intent parse leaves output untouched");
    memset(&status_sentinel, 0xa5, sizeof(status_sentinel));
    status = status_sentinel;
    require(dcent_receipt_parse_resource_status_abi1(fixture, 0, &status) !=
                DCENT_RECEIPT_FORMAT_OK,
            "failed resource status parse returns an error");
    require(memcmp(&status, &status_sentinel, sizeof(status)) == 0,
            "failed resource status parse leaves output untouched");
}

static void test_claim_records(void)
{
    char fixture[4096];
    char mutation[4096];
    char quiescence[65];
    struct dcent_receipt_claim_intent intent;
    struct dcent_receipt_claim_intent intent_sentinel;
    struct dcent_receipt_claim_status status;
    struct dcent_receipt_claim_status status_sentinel;
    size_t size;
    size_t index;

    size = build_claim_intent(fixture, sizeof(fixture), "owner_dead=true\n");
    require(dcent_receipt_parse_claim_intent_abi1(fixture, size, &intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical claim intent parses");
    require(intent.reconciler_pid == 321U &&
                intent.evidence.type == DCENT_RECEIPT_EVIDENCE_OWNER_DEATH,
            "claim intent fields decode");
    for (index = 0; index < size; ++index) {
        require(dcent_receipt_parse_claim_intent_abi1(fixture, index, &intent) !=
                    DCENT_RECEIPT_FORMAT_OK,
                "claim intent truncation refused at every byte");
    }

    size = build_claim_status(fixture, sizeof(fixture), ZERO_SHA, "claimed", 1,
                              "-", "-", "-", "");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical claimed status parses");
    require(status.ledger_generation == 1U,
            "claimed status retains its ledger generation");
    for (index = 0; index < size; ++index) {
        require(dcent_receipt_parse_claim_status_abi1(fixture, index, &status) !=
                    DCENT_RECEIPT_FORMAT_OK,
                "claim status truncation refused at every byte");
    }
    size = build_claim_status_generation(
        fixture, sizeof(fixture), ZERO_SHA, "claimed", 1U, 0U, "-", "-", "-",
        "");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "claim ledger generation zero refused");
    size = build_claim_status_generation(
        fixture, sizeof(fixture), ZERO_SHA, "claimed", 1U, 149U, "-", "-",
        "-", "");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_LIMIT,
            "claim ledger generation above 148 refused");
    body_digest("quiescent=true\n", quiescence);
    size = build_claim_status(
        fixture, sizeof(fixture), ZERO_SHA, "quiescent", 2, ZERO_SHA,
        quiescence, "maintenance-quiescence-v1", "quiescent=true\n");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical quiescent status parses");
    size = build_claim_status(
        fixture, sizeof(fixture), ZERO_SHA, "reconciling", 3, ZERO_SHA,
        quiescence, "reconciliation-begin-v1", "admitted=true\n");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical reconciling status parses");
    size = build_claim_status(
        fixture, sizeof(fixture), ZERO_SHA, "complete", 4, ZERO_SHA,
        quiescence, "reconciliation-complete-v1", "released=true\n");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical complete status parses");
    size = build_claim_status(
        fixture, sizeof(fixture), ZERO_SHA, "blocked", 2, ZERO_SHA, "-",
        "reconciliation-blocked-v1", "reason=owner_alive\n");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical claimed-to-blocked status parses");
    size = build_claim_status(
        fixture, sizeof(fixture), ZERO_SHA, "blocked", 3, ZERO_SHA,
        quiescence, "reconciliation-blocked-v1", "reason=drift\n");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical quiescent-to-blocked status parses");
    size = build_claim_status(
        fixture, sizeof(fixture), ZERO_SHA, "blocked", 4, ZERO_SHA,
        quiescence, "reconciliation-blocked-v1", "reason=conflict\n");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "canonical reconciling-to-blocked status parses");

    size = build_claim_status(
        fixture, sizeof(fixture), ZERO_SHA, "quiescent", 2, ZERO_SHA,
        ZERO_SHA, "maintenance-quiescence-v1", "quiescent=true\n");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "quiescence digest drift refused");
    size = build_claim_status(
        fixture, sizeof(fixture), ZERO_SHA, "reconciling", 3, ZERO_SHA,
        quiescence, "-", "");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "reconciling status requires begin evidence");
    size = build_claim_status(
        fixture, sizeof(fixture), ZERO_SHA, "blocked", 2, ZERO_SHA,
        quiescence, "reconciliation-blocked-v1", "reason=bad\n");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "revision-2 blocked status refuses premature quiescence");
    size = build_claim_status(
        fixture, sizeof(fixture), ZERO_SHA, "claimed", 1, ZERO_SHA, "-", "-",
        "");
    require(dcent_receipt_parse_claim_status_abi1(fixture, size, &status) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "initial claim refuses previous digest");

    size = build_claim_intent(fixture, sizeof(fixture), "owner_dead=true\n");
    memcpy(mutation, fixture, size);
    {
        char *type = strstr(mutation, "owner-death-v1");
        require(type != NULL, "claim evidence type mutation exists");
        memcpy(type, "node-intent-v1", strlen("node-intent-v1"));
    }
    require(dcent_receipt_parse_claim_intent_abi1(mutation, size, &intent) !=
                DCENT_RECEIPT_FORMAT_OK,
            "claim intent refuses resource evidence type");
    memset(&intent_sentinel, 0xa5, sizeof(intent_sentinel));
    intent = intent_sentinel;
    require(dcent_receipt_parse_claim_intent_abi1(fixture, 0, &intent) !=
                DCENT_RECEIPT_FORMAT_OK,
            "failed claim intent parse returns an error");
    require(memcmp(&intent, &intent_sentinel, sizeof(intent)) == 0,
            "failed claim intent parse leaves output untouched");
    memset(&status_sentinel, 0xa5, sizeof(status_sentinel));
    status = status_sentinel;
    require(dcent_receipt_parse_claim_status_abi1(fixture, 0, &status) !=
                DCENT_RECEIPT_FORMAT_OK,
            "failed claim status parse returns an error");
    require(memcmp(&status, &status_sentinel, sizeof(status)) == 0,
            "failed claim status parse leaves output untouched");
}

static void parse_resource_intent_or_die(
    char *buffer, size_t size, struct dcent_receipt_resource_intent *intent)
{
    require(dcent_receipt_parse_resource_intent_abi1(buffer, size, intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "chain resource intent parses");
}

static void parse_resource_status_or_die(
    char *buffer, size_t size, struct dcent_receipt_resource_status *status)
{
    require(dcent_receipt_parse_resource_status_abi1(buffer, size, status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "chain resource status parses");
}

static void parse_claim_status_or_die(
    char *buffer, size_t size, struct dcent_receipt_claim_status *status)
{
    require(dcent_receipt_parse_claim_status_abi1(buffer, size, status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "chain claim status parses");
}

static void test_chains_and_summary(void)
{
    static char binding_buffer[4096];
    static char resource_intent_buffer[4096];
    static char resource_status_buffers[4][4096];
    static char active_intent_buffer[4096];
    static char active_status_buffers[2][4096];
    static char reconciled_intent_buffer[4096];
    static char reconciled_status_buffers[2][4096];
    static char authority_intent_buffer[4096];
    static char authority_status_buffers[3][4096];
    static char wrong_status_buffer[4096];
    static char claim_intent_buffer[4096];
    static char claim_status_buffers[4][4096];
    struct dcent_receipt_binding binding;
    struct dcent_receipt_binding_anchor anchor;
    struct dcent_receipt_resource_intent resource_intent;
    struct dcent_receipt_resource_status resource_status;
    struct dcent_receipt_resource_chain released;
    struct dcent_receipt_resource_chain active;
    struct dcent_receipt_resource_chain reconciled;
    struct dcent_receipt_resource_chain authority;
    struct dcent_receipt_resource_chain snapshot;
    struct dcent_receipt_resource_chain duplicate[2];
    struct dcent_receipt_resource_chain maximum[32];
    struct dcent_receipt_resource_chain over_limit[33];
    struct dcent_receipt_claim_intent claim_intent;
    struct dcent_receipt_claim_status claim_status;
    struct dcent_receipt_claim_chain claim;
    struct dcent_receipt_claim_chain claim_quiescent;
    struct dcent_receipt_claim_chain claim_reconciling;
    struct dcent_receipt_binding_anchor wrong_binding;
    struct dcent_receipt_resource_intent wrong_intent;
    struct dcent_receipt_claim_intent wrong_claim_intent;
    struct dcent_receipt_claim_chain wrong_claim;
    struct dcent_receipt_claim_chain wrong_claim_before;
    struct dcent_receipt_resource_chain wrong_resource;
    char intent_hex[65];
    char previous_hex[65];
    char claim_intent_hex[65];
    char quiescence_hex[65];
    char binding_hex[65];
    size_t size;

    size = build_binding(
        binding_buffer, sizeof(binding_buffer), TX_ID,
        "abcdef12-3456-7890-abcd-ef1234567890", "123", "456", "1:22",
        "/run/dcentos-sysupgrade.lock", "2:33",
        "/run/dcentos-sysupgrade.lock/ledger");
    require(dcent_receipt_parse_binding_abi1(binding_buffer, size, &binding) ==
                DCENT_RECEIPT_FORMAT_OK,
            "chain binding parses");
    require(dcent_receipt_binding_anchor_init(&anchor, &binding) ==
                DCENT_RECEIPT_FORMAT_OK,
            "chain binding is retained in an owned anchor");
    bytes_to_hex(binding.record_sha256, binding_hex);
    fixture_binding_sha256 = binding_hex;

    size = build_resource_intent(
        resource_intent_buffer, sizeof(resource_intent_buffer), "attachment",
        "ubi-1", "created", "7", "1", "-", "attachment-intent-v1",
        "absent=true\n");
    parse_resource_intent_or_die(resource_intent_buffer, size,
                                 &resource_intent);
    wrong_intent = resource_intent;
    wrong_intent.binding_sha256.bytes[0] ^= 1U;
    memset(&snapshot, 0xa5, sizeof(snapshot));
    authority = snapshot;
    require(dcent_receipt_resource_chain_begin(
                &authority, &anchor, &wrong_intent) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "resource chain refuses intent bound to another binding digest");
    require(memcmp(&authority, &snapshot, sizeof(authority)) == 0,
            "failed resource chain begin leaves output untouched");
    wrong_intent = resource_intent;
    wrong_intent.transaction_id.data = (const unsigned char *)"Other-2";
    wrong_intent.transaction_id.size = strlen("Other-2");
    require(dcent_receipt_resource_chain_begin(
                &authority, &anchor, &wrong_intent) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "resource chain refuses intent transaction outside binding");
    require(dcent_receipt_resource_chain_begin(&released, &anchor,
                                               &resource_intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "resource chain begins from intent");
    bytes_to_hex(resource_intent.record_sha256, intent_hex);

    size = build_resource_status(resource_status_buffers[0], 4096,
                                 "attachment", "ubi-1", intent_hex, "pending",
                                 1, "-", "owner", TX_ID, "-", "");
    parse_resource_status_or_die(resource_status_buffers[0], size,
                                 &resource_status);
    require(dcent_receipt_resource_chain_add(&released, &resource_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "pending status starts resource chain");
    bytes_to_hex(resource_status.record_sha256, previous_hex);
    snapshot = released;
    size = build_resource_status_generation(
        wrong_status_buffer, sizeof(wrong_status_buffer), "attachment",
        "ubi-1", intent_hex, "active", 2U, 1U, previous_hex, "owner", TX_ID,
        "attachment-active-v1", "attached=true\n");
    parse_resource_status_or_die(wrong_status_buffer, size, &resource_status);
    require(dcent_receipt_resource_chain_add(&released, &resource_status) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "resource chain refuses non-increasing ledger generation");
    require(memcmp(&released, &snapshot, sizeof(released)) == 0,
            "failed resource generation update preserves chain state");
    size = build_resource_status(
        resource_status_buffers[1], 4096, "attachment", "ubi-1", intent_hex,
        "active", 2, previous_hex, "owner", TX_ID, "attachment-active-v1",
        "attached=true\n");
    parse_resource_status_or_die(resource_status_buffers[1], size,
                                 &resource_status);
    require(dcent_receipt_resource_chain_add(&released, &resource_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "active status extends resource chain");
    bytes_to_hex(resource_status.record_sha256, previous_hex);
    size = build_resource_status(
        resource_status_buffers[2], 4096, "attachment", "ubi-1", intent_hex,
        "release-pending", 3, previous_hex, "owner", TX_ID,
        "attachment-release-pending-v1", "same_object=true\n");
    parse_resource_status_or_die(resource_status_buffers[2], size,
                                 &resource_status);
    require(dcent_receipt_resource_chain_add(&released, &resource_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "release-pending status extends resource chain");
    bytes_to_hex(resource_status.record_sha256, previous_hex);
    size = build_resource_status(
        resource_status_buffers[3], 4096, "attachment", "ubi-1", intent_hex,
        "released", 4, previous_hex, "owner", TX_ID,
        "attachment-released-v1", "absent=true\n");
    parse_resource_status_or_die(resource_status_buffers[3], size,
                                 &resource_status);
    require(dcent_receipt_resource_chain_add(&released, &resource_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "released status completes resource chain");
    require(dcent_receipt_resource_chain_finish(&released) ==
                DCENT_RECEIPT_FORMAT_OK,
            "resource chain finishes");

    size = build_claim_intent(claim_intent_buffer, sizeof(claim_intent_buffer),
                              "owner_dead=true\n");
    require(dcent_receipt_parse_claim_intent_abi1(claim_intent_buffer, size,
                                                  &claim_intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "chain claim intent parses");
    wrong_claim_intent = claim_intent;
    wrong_claim_intent.binding_sha256.bytes[0] ^= 1U;
    memset(&wrong_claim, 0xa5, sizeof(wrong_claim));
    require(dcent_receipt_claim_chain_begin(
                &wrong_claim, &anchor, &wrong_claim_intent) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "claim chain refuses intent bound to another binding digest");
    wrong_claim_intent = claim_intent;
    wrong_claim_intent.transaction_id.data =
        (const unsigned char *)"Other-2";
    wrong_claim_intent.transaction_id.size = strlen("Other-2");
    require(dcent_receipt_claim_chain_begin(
                &wrong_claim, &anchor, &wrong_claim_intent) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "claim chain refuses intent transaction outside binding");
    wrong_claim_intent = claim_intent;
    wrong_claim_intent.reconciler_boot_id.data =
        (const unsigned char *)"00000000-0000-4000-8000-000000000000";
    wrong_claim_intent.reconciler_boot_id.size = 36U;
    memset(&wrong_claim, 0xa5, sizeof(wrong_claim));
    wrong_claim_before = wrong_claim;
    require(dcent_receipt_claim_chain_begin(
                &wrong_claim, &anchor, &wrong_claim_intent) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "claim chain refuses reconciler from another boot");
    require(memcmp(&wrong_claim, &wrong_claim_before, sizeof(wrong_claim)) ==
                0,
            "cross-boot claim refusal leaves output untouched");
    wrong_claim_intent = claim_intent;
    wrong_claim_intent.reconciler_pid = 0U;
    memset(&wrong_claim, 0xa5, sizeof(wrong_claim));
    require(dcent_receipt_claim_chain_begin(
                &wrong_claim, &anchor, &wrong_claim_intent) ==
                DCENT_RECEIPT_FORMAT_MALFORMED,
            "claim chain refuses missing reconciler process authority");
    wrong_claim_intent = claim_intent;
    wrong_claim_intent.evidence.digest.present = false;
    require(dcent_receipt_claim_chain_begin(
                &wrong_claim, &anchor, &wrong_claim_intent) ==
                DCENT_RECEIPT_FORMAT_MALFORMED,
            "claim chain refuses missing owner-death evidence authority");
    require(dcent_receipt_claim_chain_begin(&claim, &anchor, &claim_intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "claim chain begins");
    require(claim.reconciler_boot_id.size == 36U &&
                memcmp(claim.reconciler_boot_id.bytes,
                       "abcdef12-3456-7890-abcd-ef1234567890", 36U) == 0 &&
                claim.reconciler_pid == 321U &&
                claim.reconciler_starttime == 987654U &&
                claim.reconciler_mount_namespace.device == 3U &&
                claim.reconciler_mount_namespace.inode == 77U &&
                claim.maintenance_lock_path.size ==
                    strlen("/run/dcentos-maintenance.lock") &&
                memcmp(claim.maintenance_lock_path.bytes,
                       "/run/dcentos-maintenance.lock",
                       claim.maintenance_lock_path.size) == 0 &&
                claim.maintenance_lock_device_inode.device == 4U &&
                claim.maintenance_lock_device_inode.inode == 88U &&
                claim.owner_death_evidence_sha256.present,
            "claim chain owns complete reconciler authority");
    bytes_to_hex(claim_intent.record_sha256, claim_intent_hex);
    size = build_claim_status(claim_status_buffers[0], 4096, claim_intent_hex,
                              "claimed", 1, "-", "-", "-", "");
    parse_claim_status_or_die(claim_status_buffers[0], size, &claim_status);
    require(dcent_receipt_claim_chain_add(&claim, &claim_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "claimed status starts claim chain");
    bytes_to_hex(claim_status.record_sha256, previous_hex);
    claim_quiescent = claim;
    body_digest("quiescent=true\n", quiescence_hex);
    size = build_claim_status_generation(
        wrong_status_buffer, sizeof(wrong_status_buffer), claim_intent_hex,
        "quiescent", 2U, 1U, previous_hex, quiescence_hex,
        "maintenance-quiescence-v1", "quiescent=true\n");
    parse_claim_status_or_die(wrong_status_buffer, size, &claim_status);
    require(dcent_receipt_claim_chain_add(&claim, &claim_status) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "claim chain refuses non-increasing ledger generation");
    require(memcmp(&claim, &claim_quiescent, sizeof(claim)) == 0,
            "failed claim generation update preserves chain state");
    size = build_claim_status(
        claim_status_buffers[1], 4096, claim_intent_hex, "quiescent", 2,
        previous_hex, quiescence_hex, "maintenance-quiescence-v1",
        "quiescent=true\n");
    parse_claim_status_or_die(claim_status_buffers[1], size, &claim_status);
    require(dcent_receipt_claim_chain_add(&claim, &claim_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "quiescent status extends claim chain");
    claim_quiescent = claim;
    bytes_to_hex(claim_status.record_sha256, previous_hex);
    size = build_claim_status(
        claim_status_buffers[2], 4096, claim_intent_hex, "reconciling", 3,
        previous_hex, quiescence_hex, "reconciliation-begin-v1",
        "admitted=true\n");
    parse_claim_status_or_die(claim_status_buffers[2], size, &claim_status);
    require(dcent_receipt_claim_chain_add(&claim, &claim_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "reconciling status extends claim chain");
    claim_reconciling = claim;
    bytes_to_hex(claim_status.record_sha256, previous_hex);
    size = build_claim_status(
        claim_status_buffers[3], 4096, claim_intent_hex, "complete", 4,
        previous_hex, quiescence_hex, "reconciliation-complete-v1",
        "released=true\n");
    parse_claim_status_or_die(claim_status_buffers[3], size, &claim_status);
    require(dcent_receipt_claim_chain_add(&claim, &claim_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "complete status extends claim chain");
    require(dcent_receipt_claim_chain_finish(&claim) ==
                DCENT_RECEIPT_FORMAT_OK,
            "claim chain finishes");
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &released, 1, &claim, 4096) ==
                DCENT_RECEIPT_FORMAT_OK,
            "complete claim accepts fully released resource set");
    require(dcent_receipt_ledger_validate_summary(
                &anchor, NULL, 0, &claim, 1024) ==
                DCENT_RECEIPT_FORMAT_OK,
            "complete claim accepts vacuously released empty resource set");
    require(dcent_receipt_ledger_validate_summary(
                NULL, &released, 1, &claim, 4096) ==
                DCENT_RECEIPT_FORMAT_MALFORMED,
            "ledger summary always requires an admitted binding");
    wrong_binding = anchor;
    wrong_binding.record_sha256[0] ^= 1U;
    require(dcent_receipt_ledger_validate_summary(
                &wrong_binding, &released, 1, &claim, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "internally consistent chains cannot substitute another binding");
    wrong_binding = anchor;
    wrong_binding.transaction_id.size = strlen("Other-2");
    memcpy(wrong_binding.transaction_id.bytes, "Other-2", strlen("Other-2"));
    require(dcent_receipt_ledger_validate_summary(
                &wrong_binding, &released, 1, &claim, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "ledger summary authenticates binding transaction identity");
    wrong_claim = claim;
    wrong_claim.binding_sha256.bytes[0] ^= 1U;
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &released, 1, &wrong_claim, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "ledger summary refuses a claim from another binding");
    wrong_claim = claim;
    wrong_claim.claim_id.size = DCENT_RECEIPT_MAX_ID + 1U;
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &released, 1, &wrong_claim, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "ledger summary bounds corrupted owned claim identifiers");

    size = build_resource_intent(
        active_intent_buffer, sizeof(active_intent_buffer), "attachment",
        "ubi-2", "created", "8", "1", "-", "attachment-intent-v1",
        "absent=true\n");
    parse_resource_intent_or_die(active_intent_buffer, size, &resource_intent);
    require(dcent_receipt_resource_chain_begin(&active, &anchor,
                                               &resource_intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "active resource chain begins");
    bytes_to_hex(resource_intent.record_sha256, intent_hex);
    size = build_resource_status(active_status_buffers[0], 4096, "attachment",
                                 "ubi-2", intent_hex, "pending", 1, "-",
                                 "owner", TX_ID, "-", "");
    parse_resource_status_or_die(active_status_buffers[0], size,
                                 &resource_status);
    require(dcent_receipt_resource_chain_add(&active, &resource_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "active resource pending status adds");
    bytes_to_hex(resource_status.record_sha256, previous_hex);
    size = build_resource_status(
        active_status_buffers[1], 4096, "attachment", "ubi-2", intent_hex,
        "active", 2, previous_hex, "owner", TX_ID, "attachment-active-v1",
        "attached=true\n");
    parse_resource_status_or_die(active_status_buffers[1], size,
                                 &resource_status);
    require(dcent_receipt_resource_chain_add(&active, &resource_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "active resource status adds");
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &active, 1, &claim, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "complete claim refuses active resource");
    wrong_resource = released;
    wrong_resource.latest_phase = DCENT_RECEIPT_RESOURCE_PENDING;
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &wrong_resource, 1, &claim, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "complete claim refuses pending resource");
    wrong_resource.latest_phase = DCENT_RECEIPT_RESOURCE_RELEASE_PENDING;
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &wrong_resource, 1, &claim, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "complete claim refuses release-pending resource");
    wrong_resource.latest_phase = DCENT_RECEIPT_RESOURCE_CONFLICT;
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &wrong_resource, 1, &claim, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "complete claim refuses conflicted resource");
    wrong_resource = released;
    wrong_resource.resource_id.size = DCENT_RECEIPT_MAX_ID + 1U;
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &wrong_resource, 1, NULL, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "ledger summary bounds corrupted owned resource identifiers");

    duplicate[0] = released;
    duplicate[1] = released;
    require(dcent_receipt_ledger_validate_summary(
                &anchor, duplicate, 2, NULL, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "duplicate kind/id resource refused");
    memset(over_limit, 0, sizeof(over_limit));
    require(dcent_receipt_ledger_validate_summary(
                &anchor, over_limit, 33, NULL, 0) ==
                DCENT_RECEIPT_FORMAT_LIMIT,
            "33rd resource refused before traversal");
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &released, 1, NULL, 393217U) ==
                DCENT_RECEIPT_FORMAT_LIMIT,
            "aggregate receipt storage above 384 KiB refused");
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &released, 1, NULL, 393216U) ==
                DCENT_RECEIPT_FORMAT_OK,
            "exact 384 KiB aggregate receipt boundary accepted");
    for (size = 0; size < 32U; ++size) {
        maximum[size] = released;
        maximum[size].resource_id.size = 3U;
        maximum[size].resource_id.bytes[0] = 'r';
        maximum[size].resource_id.bytes[1] =
            (unsigned char)('0' + (size / 10U));
        maximum[size].resource_id.bytes[2] =
            (unsigned char)('0' + (size % 10U));
    }
    require(dcent_receipt_ledger_validate_summary(
                &anchor, maximum, 32, NULL, 393216U) ==
                DCENT_RECEIPT_FORMAT_OK,
            "exact 32-resource ledger boundary accepted");

    size = build_resource_intent(
        reconciled_intent_buffer, sizeof(reconciled_intent_buffer),
        "attachment", "ubi-3", "created", "9", "1", "-",
        "attachment-intent-v1", "absent=true\n");
    parse_resource_intent_or_die(reconciled_intent_buffer, size,
                                 &resource_intent);
    require(dcent_receipt_resource_chain_begin(&reconciled, &anchor,
                                               &resource_intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "reconciled resource chain begins");
    bytes_to_hex(resource_intent.record_sha256, intent_hex);
    size = build_resource_status(
        reconciled_status_buffers[0], 4096, "attachment", "ubi-3", intent_hex,
        "pending", 1, "-", "owner", TX_ID, "-", "");
    parse_resource_status_or_die(reconciled_status_buffers[0], size,
                                 &resource_status);
    require(dcent_receipt_resource_chain_add(&reconciled, &resource_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "reconciled resource pending status adds");
    bytes_to_hex(resource_status.record_sha256, previous_hex);
    size = build_resource_status(
        reconciled_status_buffers[1], 4096, "attachment", "ubi-3", intent_hex,
        "conflict", 2, previous_hex, "reconciler", CLAIM_ID,
        "attachment-conflict-v1", "ambiguous=true\n");
    parse_resource_status_or_die(reconciled_status_buffers[1], size,
                                 &resource_status);
    require(dcent_receipt_resource_chain_add(&reconciled, &resource_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "reconciler resource status adds provisionally");
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &reconciled, 1, &claim_reconciling, 4096) ==
                DCENT_RECEIPT_FORMAT_OK,
            "reconciler status requires exact reconciling claim history");
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &reconciled, 1, NULL, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "reconciler status without claim refused");
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &reconciled, 1, &claim_quiescent, 4096) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "reconciler status before reconciling revision refused");

    size = build_resource_intent(
        authority_intent_buffer, sizeof(authority_intent_buffer), "mount",
        "handoff", "borrowed", "/dev/ubi1_2", "/tmp/inactive-data", "rw",
        "mount-intent-v1", "observed=true\n");
    parse_resource_intent_or_die(authority_intent_buffer, size,
                                 &resource_intent);
    require(dcent_receipt_resource_chain_begin(
                &authority, &anchor, &resource_intent) ==
                DCENT_RECEIPT_FORMAT_OK,
            "authority handoff chain begins");
    bytes_to_hex(resource_intent.record_sha256, intent_hex);
    size = build_resource_status(
        authority_status_buffers[0], 4096, "mount", "handoff", intent_hex,
        "pending", 1, "-", "owner", TX_ID, "-", "");
    parse_resource_status_or_die(authority_status_buffers[0], size,
                                 &resource_status);
    require(dcent_receipt_resource_chain_add(&authority, &resource_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "authority handoff begins with owner");
    bytes_to_hex(resource_status.record_sha256, previous_hex);
    size = build_resource_status(
        authority_status_buffers[1], 4096, "mount", "handoff", intent_hex,
        "active", 2, previous_hex, "reconciler", CLAIM_ID,
        "mount-active-v1", "mounted=true\n");
    parse_resource_status_or_die(authority_status_buffers[1], size,
                                 &resource_status);
    require(dcent_receipt_resource_chain_add(&authority, &resource_status) ==
                DCENT_RECEIPT_FORMAT_OK,
            "reconciler takes exclusive resource authority");
    snapshot = authority;
    bytes_to_hex(resource_status.record_sha256, previous_hex);
    size = build_resource_status(
        authority_status_buffers[2], 4096, "mount", "handoff", intent_hex,
        "conflict", 3, previous_hex, "owner", TX_ID, "mount-conflict-v1",
        "ambiguous=true\n");
    parse_resource_status_or_die(authority_status_buffers[2], size,
                                 &resource_status);
    require(dcent_receipt_resource_chain_add(&authority, &resource_status) ==
                DCENT_RECEIPT_FORMAT_SEMANTIC,
            "stale owner cannot regain authority after reconciliation starts");
    require(memcmp(&authority, &snapshot, sizeof(authority)) == 0,
            "rejected authority reversion leaves chain untouched");

    size = build_resource_status(
        wrong_status_buffer, sizeof(wrong_status_buffer), "attachment", "ubi-2",
        intent_hex, "active", 2, ZERO_SHA, "owner", TX_ID,
        "attachment-active-v1", "attached=true\n");
    parse_resource_status_or_die(wrong_status_buffer, size, &resource_status);
    {
        struct dcent_receipt_resource_chain fresh;

        parse_resource_intent_or_die(active_intent_buffer,
                                     strlen(active_intent_buffer),
                                     &resource_intent);
        require(dcent_receipt_resource_chain_begin(
                    &fresh, &anchor, &resource_intent) ==
                    DCENT_RECEIPT_FORMAT_OK,
                "wrong-link chain begins");
        require(dcent_receipt_resource_chain_add(&fresh, &resource_status) ==
                    DCENT_RECEIPT_FORMAT_SEMANTIC,
                "resource chain refuses skipped initial status");
    }
    memset(resource_intent_buffer, 0xa5, sizeof(resource_intent_buffer));
    memset(binding_buffer, 0xa5, sizeof(binding_buffer));
    memset(resource_status_buffers, 0xa5, sizeof(resource_status_buffers));
    memset(claim_intent_buffer, 0xa5, sizeof(claim_intent_buffer));
    memset(claim_status_buffers, 0xa5, sizeof(claim_status_buffers));
    memset(reconciled_intent_buffer, 0xa5, sizeof(reconciled_intent_buffer));
    memset(reconciled_status_buffers, 0xa5,
           sizeof(reconciled_status_buffers));
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &released, 1, &claim, 4096) ==
                DCENT_RECEIPT_FORMAT_OK,
            "chain and binding anchors own IDs after parser buffers are reused");
    require(anchor.boot_id.size == 36U && anchor.owner_pid == 123U &&
                anchor.owner_starttime == 456U &&
                anchor.transaction_lock_path.size ==
                    strlen("/run/dcentos-sysupgrade.lock") &&
                anchor.ledger_path.size ==
                    strlen("/run/dcentos-sysupgrade.lock/ledger") &&
                anchor.ledger_device_inode.device == 5U &&
                anchor.ledger_device_inode.inode == 99U,
            "binding anchor owns the complete process and filesystem authority");
    require(dcent_receipt_ledger_validate_summary(
                &anchor, &reconciled, 1, &claim_reconciling, 4096) ==
                DCENT_RECEIPT_FORMAT_OK,
            "reconciler identity survives parser scratch-buffer reuse");
    require(claim.reconciler_boot_id.size == 36U &&
                memcmp(claim.reconciler_boot_id.bytes,
                       "abcdef12-3456-7890-abcd-ef1234567890", 36U) == 0 &&
                claim.reconciler_pid == 321U &&
                claim.reconciler_starttime == 987654U &&
                claim.maintenance_lock_path.size ==
                    strlen("/run/dcentos-maintenance.lock") &&
                claim.owner_death_evidence_sha256.present,
            "claim authority survives parser scratch-buffer reuse");
    fixture_binding_sha256 = ZERO_SHA;
}

static int parse_corpus_file(const char *kind, const char *path)
{
    unsigned char buffer[DCENT_RECEIPT_MAX_FILE];
    struct dcent_receipt_binding binding;
    struct dcent_receipt_resource_intent resource_intent;
    struct dcent_receipt_resource_status resource_status;
    struct dcent_receipt_claim_intent claim_intent;
    struct dcent_receipt_claim_status claim_status;
    struct dcent_receipt_lock_owner lock_owner;
    struct dcent_receipt_transaction_phase_status phase_status;
    FILE *file;
    size_t size;
    int extra;

    file = fopen(path, "rb");
    if (file == NULL)
        return 1;
    size = fread(buffer, 1, sizeof(buffer), file);
    extra = fgetc(file);
    if (ferror(file) || fclose(file) != 0 || extra != EOF)
        return 1;
    if (strcmp(kind, "binding") == 0)
        return dcent_receipt_parse_binding_abi1(buffer, size, &binding) !=
               DCENT_RECEIPT_FORMAT_OK;
    if (strcmp(kind, "lock-owner") == 0)
        return dcent_receipt_parse_lock_owner_v3(buffer, size, &lock_owner) !=
               DCENT_RECEIPT_FORMAT_OK;
    if (strcmp(kind, "resource-intent") == 0)
        return dcent_receipt_parse_resource_intent_abi1(
                   buffer, size, &resource_intent) != DCENT_RECEIPT_FORMAT_OK;
    if (strcmp(kind, "resource-status") == 0)
        return dcent_receipt_parse_resource_status_abi1(
                   buffer, size, &resource_status) != DCENT_RECEIPT_FORMAT_OK;
    if (strcmp(kind, "claim-intent") == 0)
        return dcent_receipt_parse_claim_intent_abi1(buffer, size,
                                                     &claim_intent) !=
               DCENT_RECEIPT_FORMAT_OK;
    if (strcmp(kind, "claim-status") == 0)
        return dcent_receipt_parse_claim_status_abi1(buffer, size,
                                                     &claim_status) !=
               DCENT_RECEIPT_FORMAT_OK;
    if (strcmp(kind, "phase-status") == 0)
        return dcent_receipt_parse_transaction_phase_status_abi2(
                   buffer, size, &phase_status) != DCENT_RECEIPT_FORMAT_OK;
    return 2;
}

int main(int argc, char **argv)
{
    if (argc == 4 && strcmp(argv[1], "--parse") == 0)
        return parse_corpus_file(argv[2], argv[3]);
    if (argc != 1) {
        fprintf(stderr, "usage: %s [--parse KIND FILE]\n", argv[0]);
        return 2;
    }
    test_registry();
    test_binding();
    test_lock_owner();
    test_transaction_phase_records();
    test_resource_records();
    test_claim_records();
    test_chains_and_summary();
    printf("dcentos-receipt ABI1 parser tests: %u assertions\n", assertions);
    return 0;
}
