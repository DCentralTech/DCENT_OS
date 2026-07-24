/* SPDX-License-Identifier: GPL-3.0-or-later */
#ifndef DCENTOS_RECEIPT_STORE_H
#define DCENTOS_RECEIPT_STORE_H

#include "receipt_format.h"

#include <stdbool.h>
#include <stddef.h>

#define DCENT_RECEIPT_STORE_ERROR_CONTEXT 96U

enum dcent_receipt_store_result {
    DCENT_RECEIPT_STORE_OK = 0,
    DCENT_RECEIPT_STORE_INVALID_ARGUMENT,
    DCENT_RECEIPT_STORE_IO,
    DCENT_RECEIPT_STORE_UNSAFE_METADATA,
    DCENT_RECEIPT_STORE_LAYOUT,
    DCENT_RECEIPT_STORE_FORMAT,
    DCENT_RECEIPT_STORE_BINDING_MISMATCH,
    DCENT_RECEIPT_STORE_LIMIT,
    DCENT_RECEIPT_STORE_RACE,
};

struct dcent_receipt_store_error {
    enum dcent_receipt_store_result result;
    int system_errno;
    int format_result;
    char context[DCENT_RECEIPT_STORE_ERROR_CONTEXT];
};

struct dcent_receipt_ledger_snapshot {
    struct dcent_receipt_lock_anchor lock;
    struct dcent_receipt_binding_anchor binding;
    struct dcent_receipt_resource_chain
        resources[DCENT_RECEIPT_MAX_RESOURCES];
    size_t resource_count;
    bool claim_present;
    struct dcent_receipt_claim_chain claim;
    size_t aggregate_bytes;
};

/*
 * Read a structurally clean ABI1 forensic snapshot through a caller-retained
 * lock-directory
 * descriptor. The scanner opens only direct children with descriptor-relative
 * Linux 4.4 primitives, performs two complete scans, and never mutates the
 * filesystem. This function does not prove that the descriptor still has the
 * canonical /run parent/name, that an owner or claimant is live, that the
 * current boot and mount namespace match, or that a mutation lease is held.
 * The caller must establish and retain those independent authorities for as
 * long as it relies on the result. A forensic snapshot is never mutation
 * permission.
 *
 * The output snapshot is failure-atomic. It is left byte-for-byte untouched on
 * every non-OK result. The optional error object receives owned diagnostic
 * context and contains no borrowed directory-entry pointers.
 */
enum dcent_receipt_store_result dcent_receipt_store_scan_forensic_abi1(
    int lock_directory_fd, struct dcent_receipt_ledger_snapshot *out,
    struct dcent_receipt_store_error *error);

const char *dcent_receipt_store_result_name(
    enum dcent_receipt_store_result result);

#ifdef DCENT_RECEIPT_STORE_TESTING
enum dcent_receipt_store_test_point {
    DCENT_RECEIPT_STORE_TEST_BEFORE_OPEN = 1,
    DCENT_RECEIPT_STORE_TEST_BETWEEN_PASSES = 2,
};

void dcent_receipt_store_test_hook(
    enum dcent_receipt_store_test_point point, int directory_fd,
    const char *name);
#endif

#endif
