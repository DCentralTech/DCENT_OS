/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_state.h"
#include "sha256.h"

#include <errno.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static unsigned int assertions;

static void require(bool condition, const char *label)
{
    ++assertions;
    if (!condition) {
        fprintf(stderr, "FAIL: %s\n", label);
        exit(1);
    }
}

static void require_digest(const void *input, size_t length,
                           const char *expected, const char *label)
{
    uint8_t digest[DCENT_RECEIPT_SHA256_BYTES];
    char hex[DCENT_RECEIPT_SHA256_HEX_BYTES + 1U];

    dcent_receipt_sha256(digest, input, length);
    dcent_receipt_sha256_hex(hex, digest);
    require(strcmp(hex, expected) == 0, label);
}

static void test_sha256(void)
{
    static const char abc[] = "abc";
    static const char long_vector[] =
        "abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
    uint8_t *million_a;

    require_digest("", 0,
                   "e3b0c44298fc1c149afbf4c8996fb924"
                   "27ae41e4649b934ca495991b7852b855",
                   "SHA-256 empty vector");
    require_digest(abc, sizeof(abc) - 1U,
                   "ba7816bf8f01cfea414140de5dae2223"
                   "b00361a396177a9cb410ff61f20015ad",
                   "SHA-256 abc vector");
    require_digest(long_vector, sizeof(long_vector) - 1U,
                   "248d6a61d20638b8e5c026930c3e6039"
                   "a33ce45964ff2167f6ecedd419db06c1",
                   "SHA-256 multi-block vector");
    million_a = malloc(1000000U);
    require(million_a != NULL, "million-byte SHA-256 fixture allocation");
    memset(million_a, 'a', 1000000U);
    require_digest(million_a, 1000000U,
                   "cdc76e5c9914fb9281a1c7e284d73e67"
                   "f1809a48a497200e046d39ccc7112cd0",
                   "SHA-256 million-a vector");
    free(million_a);
}

static void test_names(void)
{
    require(dcent_receipt_provenance_parse("created") ==
                DCENT_RECEIPT_PROVENANCE_CREATED,
            "created provenance parses");
    require(dcent_receipt_provenance_parse("borrowed") ==
                DCENT_RECEIPT_PROVENANCE_BORROWED,
            "borrowed provenance parses");
    require(dcent_receipt_provenance_parse("Created") ==
                DCENT_RECEIPT_PROVENANCE_INVALID,
            "provenance aliases are refused");
    require(dcent_receipt_resource_phase_parse("release-pending") ==
                DCENT_RECEIPT_RESOURCE_RELEASE_PENDING,
            "hyphenated resource phase parses exactly");
    require(dcent_receipt_resource_phase_parse("release_pending") ==
                DCENT_RECEIPT_RESOURCE_INVALID,
            "resource phase spelling aliases are refused");
    require(dcent_receipt_claim_phase_parse("reconciling") ==
                DCENT_RECEIPT_CLAIM_RECONCILING,
            "claim phase parses exactly");
    require(dcent_receipt_claim_phase_parse(NULL) ==
                DCENT_RECEIPT_CLAIM_INVALID,
            "null claim phase is refused");
    require(strcmp(dcent_receipt_resource_phase_name(
                       DCENT_RECEIPT_RESOURCE_ACTIVE),
                   "active") == 0,
            "resource phase has canonical spelling");
    require(dcent_receipt_resource_phase_name(
                DCENT_RECEIPT_RESOURCE_INVALID) == NULL,
            "invalid resource phase has no spelling");
}

static bool expected_resource_transition(int from, int to)
{
    return (from == DCENT_RECEIPT_RESOURCE_PENDING &&
            (to == DCENT_RECEIPT_RESOURCE_ACTIVE ||
             to == DCENT_RECEIPT_RESOURCE_RELEASED ||
             to == DCENT_RECEIPT_RESOURCE_CONFLICT)) ||
           (from == DCENT_RECEIPT_RESOURCE_ACTIVE &&
            (to == DCENT_RECEIPT_RESOURCE_RELEASE_PENDING ||
             to == DCENT_RECEIPT_RESOURCE_CONFLICT)) ||
           (from == DCENT_RECEIPT_RESOURCE_RELEASE_PENDING &&
            (to == DCENT_RECEIPT_RESOURCE_RELEASED ||
             to == DCENT_RECEIPT_RESOURCE_CONFLICT));
}

static bool expected_claim_transition(int from, int to)
{
    return (from == DCENT_RECEIPT_CLAIM_CLAIMED &&
            (to == DCENT_RECEIPT_CLAIM_QUIESCENT ||
             to == DCENT_RECEIPT_CLAIM_BLOCKED)) ||
           (from == DCENT_RECEIPT_CLAIM_QUIESCENT &&
            (to == DCENT_RECEIPT_CLAIM_RECONCILING ||
             to == DCENT_RECEIPT_CLAIM_BLOCKED)) ||
           (from == DCENT_RECEIPT_CLAIM_RECONCILING &&
            (to == DCENT_RECEIPT_CLAIM_COMPLETE ||
             to == DCENT_RECEIPT_CLAIM_BLOCKED));
}

static void test_state_matrices(void)
{
    int from;
    int to;

    for (from = DCENT_RECEIPT_RESOURCE_INVALID;
         from <= DCENT_RECEIPT_RESOURCE_CONFLICT; ++from) {
        for (to = DCENT_RECEIPT_RESOURCE_INVALID;
             to <= DCENT_RECEIPT_RESOURCE_CONFLICT; ++to) {
            require(dcent_receipt_resource_transition_valid(
                        (enum dcent_receipt_resource_phase)from,
                        (enum dcent_receipt_resource_phase)to) ==
                        expected_resource_transition(from, to),
                    "complete resource transition matrix");
        }
    }

    for (from = DCENT_RECEIPT_CLAIM_INVALID;
         from <= DCENT_RECEIPT_CLAIM_BLOCKED; ++from) {
        for (to = DCENT_RECEIPT_CLAIM_INVALID;
             to <= DCENT_RECEIPT_CLAIM_BLOCKED; ++to) {
            require(dcent_receipt_claim_transition_valid(
                        (enum dcent_receipt_claim_phase)from,
                        (enum dcent_receipt_claim_phase)to) ==
                        expected_claim_transition(from, to),
                    "complete claim transition matrix");
        }
    }

    require(dcent_receipt_resource_phase_terminal(
                DCENT_RECEIPT_RESOURCE_RELEASED),
            "released resource is terminal");
    require(dcent_receipt_resource_phase_terminal(
                DCENT_RECEIPT_RESOURCE_CONFLICT),
            "conflicted resource is terminal");
    require(!dcent_receipt_resource_phase_terminal(
                DCENT_RECEIPT_RESOURCE_ACTIVE),
            "active resource is nonterminal");
    require(dcent_receipt_claim_phase_terminal(DCENT_RECEIPT_CLAIM_COMPLETE),
            "complete claim is terminal");
    require(dcent_receipt_claim_phase_terminal(DCENT_RECEIPT_CLAIM_BLOCKED),
            "blocked claim is terminal");
    require(!dcent_receipt_claim_phase_terminal(
                DCENT_RECEIPT_CLAIM_RECONCILING),
            "reconciling claim is nonterminal");
    require(dcent_receipt_release_requires_destroy(
                DCENT_RECEIPT_PROVENANCE_CREATED),
            "created resource requires owned-object destruction");
    require(!dcent_receipt_release_requires_destroy(
                DCENT_RECEIPT_PROVENANCE_BORROWED),
            "borrowed resource is never destroyed");
    require(!dcent_receipt_release_requires_destroy(
                DCENT_RECEIPT_PROVENANCE_INVALID),
            "invalid provenance never authorizes destruction");
}

static int hash_file(const char *path)
{
    FILE *file;
    long size;
    uint8_t *data;
    uint8_t digest[DCENT_RECEIPT_SHA256_BYTES];
    char hex[DCENT_RECEIPT_SHA256_HEX_BYTES + 1U];

    file = fopen(path, "rb");
    if (file == NULL)
        return 1;
    if (fseek(file, 0, SEEK_END) != 0 || (size = ftell(file)) < 0 ||
        size > 2L * 1024L * 1024L || fseek(file, 0, SEEK_SET) != 0) {
        fclose(file);
        return 1;
    }
    data = malloc(size == 0 ? 1U : (size_t)size);
    if (data == NULL) {
        fclose(file);
        return 1;
    }
    if (size != 0 && fread(data, 1, (size_t)size, file) != (size_t)size) {
        free(data);
        fclose(file);
        return 1;
    }
    if (fclose(file) != 0) {
        free(data);
        return 1;
    }
    dcent_receipt_sha256(digest, data, (size_t)size);
    dcent_receipt_sha256_hex(hex, digest);
    free(data);
    puts(hex);
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 3 && strcmp(argv[1], "--sha256-file") == 0)
        return hash_file(argv[2]);
    if (argc != 1) {
        fprintf(stderr, "usage: %s [--sha256-file PATH]\n", argv[0]);
        return 2;
    }

    test_sha256();
    test_names();
    test_state_matrices();
    printf("dcentos-receipt core tests: %u assertions\n", assertions);
    return 0;
}
