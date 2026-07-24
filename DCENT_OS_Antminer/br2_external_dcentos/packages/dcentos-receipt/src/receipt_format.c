/* SPDX-License-Identifier: GPL-3.0-or-later */
#include "receipt_format.h"

#include <limits.h>
#include <string.h>

struct name_entry {
    int value;
    const char *name;
};

struct parse_cursor {
    const unsigned char *data;
    size_t size;
    size_t offset;
};

static const struct name_entry resource_kind_names[] = {
    {DCENT_RECEIPT_KIND_ATTACHMENT, "attachment"},
    {DCENT_RECEIPT_KIND_NODE, "node"},
    {DCENT_RECEIPT_KIND_MOUNT, "mount"},
    {DCENT_RECEIPT_KIND_WORKSPACE, "workspace"},
};

static const struct name_entry actor_kind_names[] = {
    {DCENT_RECEIPT_ACTOR_OWNER, "owner"},
    {DCENT_RECEIPT_ACTOR_RECONCILER, "reconciler"},
};

static const struct name_entry evidence_type_names[] = {
    {DCENT_RECEIPT_EVIDENCE_ATTACHMENT_INTENT, "attachment-intent-v1"},
    {DCENT_RECEIPT_EVIDENCE_ATTACHMENT_ACTIVE, "attachment-active-v1"},
    {DCENT_RECEIPT_EVIDENCE_ATTACHMENT_RELEASE_PENDING,
     "attachment-release-pending-v1"},
    {DCENT_RECEIPT_EVIDENCE_ATTACHMENT_RELEASED, "attachment-released-v1"},
    {DCENT_RECEIPT_EVIDENCE_ATTACHMENT_CONFLICT, "attachment-conflict-v1"},
    {DCENT_RECEIPT_EVIDENCE_NODE_INTENT, "node-intent-v1"},
    {DCENT_RECEIPT_EVIDENCE_NODE_ACTIVE, "node-active-v1"},
    {DCENT_RECEIPT_EVIDENCE_NODE_RELEASE_PENDING,
     "node-release-pending-v1"},
    {DCENT_RECEIPT_EVIDENCE_NODE_RELEASED, "node-released-v1"},
    {DCENT_RECEIPT_EVIDENCE_NODE_CONFLICT, "node-conflict-v1"},
    {DCENT_RECEIPT_EVIDENCE_MOUNT_INTENT, "mount-intent-v1"},
    {DCENT_RECEIPT_EVIDENCE_MOUNT_ACTIVE, "mount-active-v1"},
    {DCENT_RECEIPT_EVIDENCE_MOUNT_RELEASE_PENDING,
     "mount-release-pending-v1"},
    {DCENT_RECEIPT_EVIDENCE_MOUNT_RELEASED, "mount-released-v1"},
    {DCENT_RECEIPT_EVIDENCE_MOUNT_CONFLICT, "mount-conflict-v1"},
    {DCENT_RECEIPT_EVIDENCE_WORKSPACE_INTENT, "workspace-intent-v1"},
    {DCENT_RECEIPT_EVIDENCE_WORKSPACE_ACTIVE, "workspace-active-v1"},
    {DCENT_RECEIPT_EVIDENCE_WORKSPACE_RELEASE_PENDING,
     "workspace-release-pending-v1"},
    {DCENT_RECEIPT_EVIDENCE_WORKSPACE_RELEASED, "workspace-released-v1"},
    {DCENT_RECEIPT_EVIDENCE_WORKSPACE_CONFLICT, "workspace-conflict-v1"},
    {DCENT_RECEIPT_EVIDENCE_OWNER_DEATH, "owner-death-v1"},
    {DCENT_RECEIPT_EVIDENCE_MAINTENANCE_QUIESCENCE,
     "maintenance-quiescence-v1"},
    {DCENT_RECEIPT_EVIDENCE_RECONCILIATION_BEGIN,
     "reconciliation-begin-v1"},
    {DCENT_RECEIPT_EVIDENCE_RECONCILIATION_COMPLETE,
     "reconciliation-complete-v1"},
    {DCENT_RECEIPT_EVIDENCE_RECONCILIATION_BLOCKED,
     "reconciliation-blocked-v1"},
    {DCENT_RECEIPT_EVIDENCE_TRANSACTION_CLEANUP_REQUIRED,
     "transaction-cleanup-required-v1"},
    {DCENT_RECEIPT_EVIDENCE_TRANSACTION_ENV_COMMIT_ARMED,
     "transaction-env-commit-armed-v1"},
    {DCENT_RECEIPT_EVIDENCE_TRANSACTION_ENV_COMMIT_DISARMED,
     "transaction-env-commit-disarmed-v1"},
    {DCENT_RECEIPT_EVIDENCE_TRANSACTION_ENV_COMMITTED,
     "transaction-env-committed-v1"},
};

static int parse_name(const struct name_entry *entries, size_t count,
                      const char *text)
{
    size_t index;

    if (text == NULL)
        return 0;
    for (index = 0; index < count; ++index) {
        if (strcmp(entries[index].name, text) == 0)
            return entries[index].value;
    }
    return 0;
}

static int parse_name_slice(const struct name_entry *entries, size_t count,
                            struct dcent_receipt_slice value)
{
    size_t index;

    for (index = 0; index < count; ++index) {
        size_t length = strlen(entries[index].name);

        if (value.size == length &&
            memcmp(value.data, entries[index].name, length) == 0)
            return entries[index].value;
    }
    return 0;
}

static const char *lookup_name(const struct name_entry *entries, size_t count,
                               int value)
{
    size_t index;

    for (index = 0; index < count; ++index) {
        if (entries[index].value == value)
            return entries[index].name;
    }
    return NULL;
}

enum dcent_receipt_resource_kind dcent_receipt_resource_kind_parse(
    const char *text)
{
    return (enum dcent_receipt_resource_kind)parse_name(
        resource_kind_names,
        sizeof(resource_kind_names) / sizeof(resource_kind_names[0]), text);
}

const char *dcent_receipt_resource_kind_name(
    enum dcent_receipt_resource_kind value)
{
    return lookup_name(
        resource_kind_names,
        sizeof(resource_kind_names) / sizeof(resource_kind_names[0]), value);
}

enum dcent_receipt_actor_kind dcent_receipt_actor_kind_parse(const char *text)
{
    return (enum dcent_receipt_actor_kind)parse_name(
        actor_kind_names,
        sizeof(actor_kind_names) / sizeof(actor_kind_names[0]), text);
}

const char *dcent_receipt_actor_kind_name(enum dcent_receipt_actor_kind value)
{
    return lookup_name(actor_kind_names,
                       sizeof(actor_kind_names) / sizeof(actor_kind_names[0]),
                       value);
}

enum dcent_receipt_evidence_type dcent_receipt_evidence_type_parse(
    const char *text)
{
    if (text != NULL && strcmp(text, "-") == 0)
        return DCENT_RECEIPT_EVIDENCE_NONE;
    return (enum dcent_receipt_evidence_type)parse_name(
        evidence_type_names,
        sizeof(evidence_type_names) / sizeof(evidence_type_names[0]), text);
}

const char *dcent_receipt_evidence_type_name(
    enum dcent_receipt_evidence_type value)
{
    if (value == DCENT_RECEIPT_EVIDENCE_NONE)
        return "-";
    return lookup_name(
        evidence_type_names,
        sizeof(evidence_type_names) / sizeof(evidence_type_names[0]), value);
}

static bool slice_equal(struct dcent_receipt_slice left,
                        struct dcent_receipt_slice right)
{
    return left.size == right.size &&
           (left.size == 0 ||
            (left.data != NULL && right.data != NULL &&
             memcmp(left.data, right.data, left.size) == 0));
}

static bool id_valid(struct dcent_receipt_slice value);
static bool boot_id_valid(struct dcent_receipt_slice value);

static bool id_copy(struct dcent_receipt_id *destination,
                    struct dcent_receipt_slice source)
{
    if (destination == NULL || !id_valid(source))
        return false;
    destination->size = source.size;
    memcpy(destination->bytes, source.data, source.size);
    return true;
}

static bool id_equal_slice(const struct dcent_receipt_id *left,
                           struct dcent_receipt_slice right)
{
    return left != NULL && left->size <= DCENT_RECEIPT_MAX_ID &&
           right.size <= DCENT_RECEIPT_MAX_ID &&
           (right.size == 0U || right.data != NULL) &&
           left->size == right.size &&
           (right.size == 0U ||
            memcmp(left->bytes, right.data, right.size) == 0);
}

static bool id_equal(const struct dcent_receipt_id *left,
                     const struct dcent_receipt_id *right)
{
    return left != NULL && right != NULL &&
           left->size <= DCENT_RECEIPT_MAX_ID &&
           right->size <= DCENT_RECEIPT_MAX_ID && left->size == right->size &&
           (left->size == 0U ||
            memcmp(left->bytes, right->bytes, left->size) == 0);
}

static bool owned_id_valid(const struct dcent_receipt_id *value)
{
    struct dcent_receipt_slice slice;

    if (value == NULL || value->size > DCENT_RECEIPT_MAX_ID)
        return false;
    slice.data = value->bytes;
    slice.size = value->size;
    return id_valid(slice);
}

static bool owned_boot_id_valid(const struct dcent_receipt_id *value)
{
    struct dcent_receipt_slice slice;

    if (value == NULL || value->size > DCENT_RECEIPT_MAX_ID)
        return false;
    slice.data = value->bytes;
    slice.size = value->size;
    return boot_id_valid(slice);
}

static bool slice_literal(struct dcent_receipt_slice value,
                          const char *literal)
{
    size_t length = strlen(literal);

    return value.size == length &&
           (length == 0U ||
            (value.data != NULL && memcmp(value.data, literal, length) == 0));
}

static bool digest_equal(const unsigned char *left,
                         const unsigned char *right)
{
    return memcmp(left, right, DCENT_RECEIPT_SHA256_BYTES) == 0;
}

static int cursor_init(struct parse_cursor *cursor, const void *data,
                       size_t size)
{
    if (cursor == NULL || data == NULL || size == 0)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    if (size > DCENT_RECEIPT_MAX_FILE)
        return DCENT_RECEIPT_FORMAT_LIMIT;
    cursor->data = data;
    cursor->size = size;
    cursor->offset = 0;
    return DCENT_RECEIPT_FORMAT_OK;
}

static int take_line(struct parse_cursor *cursor,
                     struct dcent_receipt_slice *line)
{
    size_t start;
    size_t index;

    if (cursor == NULL || line == NULL || cursor->offset >= cursor->size)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    start = cursor->offset;
    for (index = start; index < cursor->size; ++index) {
        unsigned char byte = cursor->data[index];

        if (byte == '\n') {
            if (index - start + 1U > DCENT_RECEIPT_MAX_HEADER_LINE)
                return DCENT_RECEIPT_FORMAT_LIMIT;
            line->data = cursor->data + start;
            line->size = index - start;
            cursor->offset = index + 1U;
            return DCENT_RECEIPT_FORMAT_OK;
        }
        if (byte < 0x20U || byte > 0x7eU)
            return DCENT_RECEIPT_FORMAT_MALFORMED;
    }
    return DCENT_RECEIPT_FORMAT_MALFORMED;
}

static int take_literal_line(struct parse_cursor *cursor, const char *literal)
{
    struct dcent_receipt_slice line;
    int result = take_line(cursor, &line);

    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    return slice_literal(line, literal) ? DCENT_RECEIPT_FORMAT_OK
                                        : DCENT_RECEIPT_FORMAT_MALFORMED;
}

static int take_value(struct parse_cursor *cursor, const char *key,
                      struct dcent_receipt_slice *value)
{
    struct dcent_receipt_slice line;
    size_t key_length = strlen(key);
    int result = take_line(cursor, &line);

    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (line.size <= key_length || line.data[key_length] != '=' ||
        memcmp(line.data, key, key_length) != 0)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    value->data = line.data + key_length + 1U;
    value->size = line.size - key_length - 1U;
    return DCENT_RECEIPT_FORMAT_OK;
}

static bool ascii_alnum(unsigned char byte)
{
    return (byte >= 'A' && byte <= 'Z') ||
           (byte >= 'a' && byte <= 'z') ||
           (byte >= '0' && byte <= '9');
}

static bool id_valid(struct dcent_receipt_slice value)
{
    size_t index;

    if (value.data == NULL || value.size == 0 ||
        value.size > DCENT_RECEIPT_MAX_ID ||
        !ascii_alnum(value.data[0]))
        return false;
    for (index = 1; index < value.size; ++index) {
        unsigned char byte = value.data[index];

        if (!ascii_alnum(byte) && byte != '.' && byte != '_' && byte != '-')
            return false;
    }
    return true;
}

static bool boot_id_valid(struct dcent_receipt_slice value)
{
    size_t index;

    if (value.data == NULL || value.size != 36U)
        return false;
    for (index = 0; index < value.size; ++index) {
        unsigned char byte = value.data[index];
        bool hyphen = index == 8U || index == 13U || index == 18U ||
                      index == 23U;

        if (hyphen ? byte != '-' : !((byte >= '0' && byte <= '9') ||
                                     (byte >= 'a' && byte <= 'f')))
            return false;
    }
    return true;
}

static int parse_uint(struct dcent_receipt_slice value, uint64_t maximum,
                      uint64_t *out)
{
    uint64_t parsed = 0;
    size_t index;

    if (value.data == NULL || value.size == 0 || out == NULL ||
        (value.size > 1U && value.data[0] == '0'))
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    for (index = 0; index < value.size; ++index) {
        unsigned int digit;

        if (value.data[index] < '0' || value.data[index] > '9')
            return DCENT_RECEIPT_FORMAT_MALFORMED;
        digit = (unsigned int)(value.data[index] - '0');
        if (digit > maximum || parsed > (maximum - digit) / 10U)
            return DCENT_RECEIPT_FORMAT_LIMIT;
        parsed = parsed * 10U + digit;
    }
    *out = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

static int parse_devino(struct dcent_receipt_slice value,
                        struct dcent_receipt_devino *out)
{
    struct dcent_receipt_devino parsed;
    struct dcent_receipt_slice device;
    struct dcent_receipt_slice inode;
    size_t index;
    size_t colon = SIZE_MAX;
    int result;

    if (out == NULL)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    for (index = 0; index < value.size; ++index) {
        if (value.data[index] == ':') {
            if (colon != SIZE_MAX)
                return DCENT_RECEIPT_FORMAT_MALFORMED;
            colon = index;
        }
    }
    if (colon == SIZE_MAX || colon == 0 || colon + 1U >= value.size)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    device.data = value.data;
    device.size = colon;
    inode.data = value.data + colon + 1U;
    inode.size = value.size - colon - 1U;
    result = parse_uint(device, UINT64_MAX, &parsed.device);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_uint(inode, UINT64_MAX, &parsed.inode);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    *out = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

static bool path_character_valid(unsigned char byte)
{
    return ascii_alnum(byte) || byte == '.' || byte == '_' || byte == '/' ||
           byte == '@' || byte == ':' || byte == '+' || byte == '-';
}

static bool path_valid(struct dcent_receipt_slice value)
{
    size_t component_start;
    size_t index;

    if (value.data == NULL || value.size < 2U ||
        value.size > DCENT_RECEIPT_MAX_PATH ||
        value.data[0] != '/' || value.data[value.size - 1U] == '/')
        return false;
    component_start = 1U;
    for (index = 1U; index <= value.size; ++index) {
        if (index < value.size && !path_character_valid(value.data[index]))
            return false;
        if (index == value.size || value.data[index] == '/') {
            size_t component_size = index - component_start;

            if (component_size == 0 ||
                (component_size == 1U && value.data[component_start] == '.') ||
                (component_size == 2U && value.data[component_start] == '.' &&
                 value.data[component_start + 1U] == '.'))
                return false;
            component_start = index + 1U;
        }
    }
    return true;
}

static bool path_copy(struct dcent_receipt_path *destination,
                      struct dcent_receipt_slice source)
{
    if (destination == NULL || !path_valid(source))
        return false;
    memset(destination, 0, sizeof(*destination));
    destination->size = source.size;
    memcpy(destination->bytes, source.data, source.size);
    return true;
}

static bool owned_path_valid(const struct dcent_receipt_path *value)
{
    struct dcent_receipt_slice slice;

    if (value == NULL || value->size > DCENT_RECEIPT_MAX_PATH)
        return false;
    slice.data = value->bytes;
    slice.size = value->size;
    return path_valid(slice);
}

static int hex_value(unsigned char byte)
{
    if (byte >= '0' && byte <= '9')
        return byte - '0';
    if (byte >= 'a' && byte <= 'f')
        return byte - 'a' + 10;
    return -1;
}

static int parse_digest(struct dcent_receipt_slice value, bool allow_dash,
                        struct dcent_receipt_digest *out)
{
    struct dcent_receipt_digest parsed;
    size_t index;

    if (out == NULL)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    memset(&parsed, 0, sizeof(parsed));
    if (allow_dash && slice_literal(value, "-")) {
        *out = parsed;
        return DCENT_RECEIPT_FORMAT_OK;
    }
    if (value.size != DCENT_RECEIPT_SHA256_HEX_BYTES)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    for (index = 0; index < DCENT_RECEIPT_SHA256_BYTES; ++index) {
        int high = hex_value(value.data[index * 2U]);
        int low = hex_value(value.data[index * 2U + 1U]);

        if (high < 0 || low < 0)
            return DCENT_RECEIPT_FORMAT_MALFORMED;
        parsed.bytes[index] = (unsigned char)((high << 4) | low);
    }
    parsed.present = true;
    *out = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

static bool evidence_key_valid(struct dcent_receipt_slice key)
{
    size_t index;

    if (key.data == NULL || key.size == 0 || key.size > 48U ||
        key.data[0] < 'a' ||
        key.data[0] > 'z')
        return false;
    for (index = 1; index < key.size; ++index) {
        unsigned char byte = key.data[index];

        if (!((byte >= 'a' && byte <= 'z') ||
              (byte >= '0' && byte <= '9') || byte == '_'))
            return false;
    }
    return true;
}

static int validate_evidence_body(struct dcent_receipt_slice body)
{
    struct dcent_receipt_slice keys[DCENT_RECEIPT_MAX_EVIDENCE_LINES];
    size_t line_start = 0;
    size_t line_count = 0;
    size_t index;

    if (body.size == 0)
        return DCENT_RECEIPT_FORMAT_OK;
    if (body.data == NULL)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    if (body.size > DCENT_RECEIPT_MAX_EVIDENCE)
        return DCENT_RECEIPT_FORMAT_LIMIT;
    if (body.data[body.size - 1U] != '\n')
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    for (index = 0; index < body.size; ++index) {
        size_t key_end;
        size_t prior;

        if (body.data[index] != '\n')
            continue;
        if (line_count == DCENT_RECEIPT_MAX_EVIDENCE_LINES)
            return DCENT_RECEIPT_FORMAT_LIMIT;
        if (index == line_start)
            return DCENT_RECEIPT_FORMAT_MALFORMED;
        key_end = line_start;
        while (key_end < index && body.data[key_end] != '=')
            ++key_end;
        if (key_end == line_start || key_end + 1U >= index)
            return DCENT_RECEIPT_FORMAT_MALFORMED;
        keys[line_count].data = body.data + line_start;
        keys[line_count].size = key_end - line_start;
        if (!evidence_key_valid(keys[line_count]))
            return DCENT_RECEIPT_FORMAT_MALFORMED;
        for (prior = 0; prior < line_count; ++prior) {
            if (slice_equal(keys[prior], keys[line_count]))
                return DCENT_RECEIPT_FORMAT_SEMANTIC;
        }
        for (++key_end; key_end < index; ++key_end) {
            if (body.data[key_end] < 0x21U || body.data[key_end] > 0x7eU)
                return DCENT_RECEIPT_FORMAT_MALFORMED;
        }
        ++line_count;
        line_start = index + 1U;
    }
    return line_count == 0 ? DCENT_RECEIPT_FORMAT_MALFORMED
                           : DCENT_RECEIPT_FORMAT_OK;
}

static int parse_evidence(struct parse_cursor *cursor,
                          struct dcent_receipt_evidence *out)
{
    struct dcent_receipt_evidence parsed;
    struct dcent_receipt_slice type;
    struct dcent_receipt_slice size_text;
    struct dcent_receipt_slice digest;
    uint64_t body_size;
    unsigned char actual[DCENT_RECEIPT_SHA256_BYTES];
    int result;

    memset(&parsed, 0, sizeof(parsed));
    result = take_value(cursor, "evidence_type", &type);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (type.size == 0 || type.size > DCENT_RECEIPT_MAX_EVIDENCE_TYPE)
        return DCENT_RECEIPT_FORMAT_LIMIT;
    if (slice_literal(type, "-")) {
        parsed.type = DCENT_RECEIPT_EVIDENCE_NONE;
    } else {
        parsed.type = (enum dcent_receipt_evidence_type)parse_name_slice(
            evidence_type_names,
            sizeof(evidence_type_names) / sizeof(evidence_type_names[0]), type);
        if (parsed.type == DCENT_RECEIPT_EVIDENCE_INVALID)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
    }
    result = take_value(cursor, "evidence_size", &size_text);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_uint(size_text, DCENT_RECEIPT_MAX_EVIDENCE, &body_size);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(cursor, "evidence_sha256", &digest);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_digest(digest, true, &parsed.digest);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_literal_line(cursor, "");
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (cursor->size - cursor->offset != (size_t)body_size)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    parsed.body.data = cursor->data + cursor->offset;
    parsed.body.size = (size_t)body_size;
    cursor->offset = cursor->size;
    result = validate_evidence_body(parsed.body);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (body_size == 0) {
        if (parsed.type != DCENT_RECEIPT_EVIDENCE_NONE ||
            parsed.digest.present)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
    } else {
        if (parsed.type == DCENT_RECEIPT_EVIDENCE_NONE ||
            parsed.type == DCENT_RECEIPT_EVIDENCE_INVALID ||
            !parsed.digest.present)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        dcent_receipt_sha256(actual, parsed.body.data, parsed.body.size);
        if (!digest_equal(actual, parsed.digest.bytes))
            return DCENT_RECEIPT_FORMAT_DIGEST_MISMATCH;
    }
    *out = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

static enum dcent_receipt_resource_kind parse_resource_kind_slice(
    struct dcent_receipt_slice value)
{
    return (enum dcent_receipt_resource_kind)parse_name_slice(
        resource_kind_names,
        sizeof(resource_kind_names) / sizeof(resource_kind_names[0]), value);
}

static enum dcent_receipt_actor_kind parse_actor_kind_slice(
    struct dcent_receipt_slice value)
{
    return (enum dcent_receipt_actor_kind)parse_name_slice(
        actor_kind_names,
        sizeof(actor_kind_names) / sizeof(actor_kind_names[0]), value);
}

static bool resource_identity_valid(
    enum dcent_receipt_resource_kind kind, struct dcent_receipt_slice a,
    struct dcent_receipt_slice b, struct dcent_receipt_slice c)
{
    uint64_t number;
    struct dcent_receipt_devino devino;

    switch (kind) {
    case DCENT_RECEIPT_KIND_ATTACHMENT:
        return parse_uint(a, UINT32_MAX, &number) == DCENT_RECEIPT_FORMAT_OK &&
               parse_uint(b, UINT32_MAX, &number) == DCENT_RECEIPT_FORMAT_OK &&
               slice_literal(c, "-");
    case DCENT_RECEIPT_KIND_NODE:
    case DCENT_RECEIPT_KIND_WORKSPACE:
        return path_valid(a) &&
               parse_devino(b, &devino) == DCENT_RECEIPT_FORMAT_OK &&
               slice_literal(c, "-");
    case DCENT_RECEIPT_KIND_MOUNT:
        return path_valid(a) && path_valid(b) &&
               (slice_literal(c, "ro") || slice_literal(c, "rw"));
    case DCENT_RECEIPT_KIND_INVALID:
        return false;
    }
    return false;
}

static enum dcent_receipt_evidence_type resource_evidence_type(
    enum dcent_receipt_resource_kind kind,
    enum dcent_receipt_resource_phase phase, bool intent)
{
    static const enum dcent_receipt_evidence_type table[4][5] = {
        {DCENT_RECEIPT_EVIDENCE_ATTACHMENT_INTENT,
         DCENT_RECEIPT_EVIDENCE_ATTACHMENT_ACTIVE,
         DCENT_RECEIPT_EVIDENCE_ATTACHMENT_RELEASE_PENDING,
         DCENT_RECEIPT_EVIDENCE_ATTACHMENT_RELEASED,
         DCENT_RECEIPT_EVIDENCE_ATTACHMENT_CONFLICT},
        {DCENT_RECEIPT_EVIDENCE_NODE_INTENT,
         DCENT_RECEIPT_EVIDENCE_NODE_ACTIVE,
         DCENT_RECEIPT_EVIDENCE_NODE_RELEASE_PENDING,
         DCENT_RECEIPT_EVIDENCE_NODE_RELEASED,
         DCENT_RECEIPT_EVIDENCE_NODE_CONFLICT},
        {DCENT_RECEIPT_EVIDENCE_MOUNT_INTENT,
         DCENT_RECEIPT_EVIDENCE_MOUNT_ACTIVE,
         DCENT_RECEIPT_EVIDENCE_MOUNT_RELEASE_PENDING,
         DCENT_RECEIPT_EVIDENCE_MOUNT_RELEASED,
         DCENT_RECEIPT_EVIDENCE_MOUNT_CONFLICT},
        {DCENT_RECEIPT_EVIDENCE_WORKSPACE_INTENT,
         DCENT_RECEIPT_EVIDENCE_WORKSPACE_ACTIVE,
         DCENT_RECEIPT_EVIDENCE_WORKSPACE_RELEASE_PENDING,
         DCENT_RECEIPT_EVIDENCE_WORKSPACE_RELEASED,
         DCENT_RECEIPT_EVIDENCE_WORKSPACE_CONFLICT},
    };
    size_t kind_index;
    size_t phase_index = 0U;

    if (kind < DCENT_RECEIPT_KIND_ATTACHMENT ||
        kind > DCENT_RECEIPT_KIND_WORKSPACE)
        return DCENT_RECEIPT_EVIDENCE_INVALID;
    kind_index = (size_t)kind - (size_t)DCENT_RECEIPT_KIND_ATTACHMENT;
    if (intent)
        return table[kind_index][0];
    switch (phase) {
    case DCENT_RECEIPT_RESOURCE_ACTIVE:
        phase_index = 1U;
        break;
    case DCENT_RECEIPT_RESOURCE_RELEASE_PENDING:
        phase_index = 2U;
        break;
    case DCENT_RECEIPT_RESOURCE_RELEASED:
        phase_index = 3U;
        break;
    case DCENT_RECEIPT_RESOURCE_CONFLICT:
        phase_index = 4U;
        break;
    case DCENT_RECEIPT_RESOURCE_INVALID:
    case DCENT_RECEIPT_RESOURCE_PENDING:
        return DCENT_RECEIPT_EVIDENCE_INVALID;
    default:
        return DCENT_RECEIPT_EVIDENCE_INVALID;
    }
    return table[kind_index][phase_index];
}

static bool digest_present(const struct dcent_receipt_digest *digest)
{
    return digest != NULL && digest->present;
}

static void record_digest(const void *data, size_t size,
                          unsigned char *digest)
{
    dcent_receipt_sha256(digest, data, size);
}

static enum dcent_receipt_provenance parse_provenance_slice(
    struct dcent_receipt_slice value)
{
    if (slice_literal(value, "created"))
        return DCENT_RECEIPT_PROVENANCE_CREATED;
    if (slice_literal(value, "borrowed"))
        return DCENT_RECEIPT_PROVENANCE_BORROWED;
    return DCENT_RECEIPT_PROVENANCE_INVALID;
}

static enum dcent_receipt_resource_phase parse_resource_phase_slice(
    struct dcent_receipt_slice value)
{
    if (slice_literal(value, "pending"))
        return DCENT_RECEIPT_RESOURCE_PENDING;
    if (slice_literal(value, "active"))
        return DCENT_RECEIPT_RESOURCE_ACTIVE;
    if (slice_literal(value, "release-pending"))
        return DCENT_RECEIPT_RESOURCE_RELEASE_PENDING;
    if (slice_literal(value, "released"))
        return DCENT_RECEIPT_RESOURCE_RELEASED;
    if (slice_literal(value, "conflict"))
        return DCENT_RECEIPT_RESOURCE_CONFLICT;
    return DCENT_RECEIPT_RESOURCE_INVALID;
}

static enum dcent_receipt_claim_phase parse_claim_phase_slice(
    struct dcent_receipt_slice value)
{
    if (slice_literal(value, "claimed"))
        return DCENT_RECEIPT_CLAIM_CLAIMED;
    if (slice_literal(value, "quiescent"))
        return DCENT_RECEIPT_CLAIM_QUIESCENT;
    if (slice_literal(value, "reconciling"))
        return DCENT_RECEIPT_CLAIM_RECONCILING;
    if (slice_literal(value, "complete"))
        return DCENT_RECEIPT_CLAIM_COMPLETE;
    if (slice_literal(value, "blocked"))
        return DCENT_RECEIPT_CLAIM_BLOCKED;
    return DCENT_RECEIPT_CLAIM_INVALID;
}

static enum dcent_receipt_lock_phase parse_lock_phase_slice(
    struct dcent_receipt_slice value)
{
    if (slice_literal(value, "active"))
        return DCENT_RECEIPT_LOCK_ACTIVE;
    if (slice_literal(value, "cleanup-required"))
        return DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED;
    if (slice_literal(value, "env-commit-armed"))
        return DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED;
    if (slice_literal(value, "env-committed"))
        return DCENT_RECEIPT_LOCK_ENV_COMMITTED;
    return DCENT_RECEIPT_LOCK_INVALID;
}

static enum dcent_receipt_evidence_type transaction_phase_evidence_type(
    enum dcent_receipt_lock_phase phase)
{
    switch (phase) {
    case DCENT_RECEIPT_LOCK_ACTIVE:
        return DCENT_RECEIPT_EVIDENCE_TRANSACTION_ENV_COMMIT_DISARMED;
    case DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED:
        return DCENT_RECEIPT_EVIDENCE_TRANSACTION_CLEANUP_REQUIRED;
    case DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED:
        return DCENT_RECEIPT_EVIDENCE_TRANSACTION_ENV_COMMIT_ARMED;
    case DCENT_RECEIPT_LOCK_ENV_COMMITTED:
        return DCENT_RECEIPT_EVIDENCE_TRANSACTION_ENV_COMMITTED;
    case DCENT_RECEIPT_LOCK_INVALID:
        return DCENT_RECEIPT_EVIDENCE_INVALID;
    }
    return DCENT_RECEIPT_EVIDENCE_INVALID;
}

static bool transaction_phase_transition_valid(
    enum dcent_receipt_lock_phase from, enum dcent_receipt_lock_phase to)
{
    switch (from) {
    case DCENT_RECEIPT_LOCK_ACTIVE:
        return to == DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED ||
               to == DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED;
    case DCENT_RECEIPT_LOCK_ENV_COMMIT_ARMED:
        return to == DCENT_RECEIPT_LOCK_ACTIVE ||
               to == DCENT_RECEIPT_LOCK_ENV_COMMITTED;
    case DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED:
    case DCENT_RECEIPT_LOCK_ENV_COMMITTED:
    case DCENT_RECEIPT_LOCK_INVALID:
        return false;
    }
    return false;
}

static bool binding_ledger_path_matches(
    struct dcent_receipt_slice lock_path,
    struct dcent_receipt_slice ledger_path)
{
    static const char suffix[] = "/ledger";

    return ledger_path.size == lock_path.size + sizeof(suffix) - 1U &&
           memcmp(ledger_path.data, lock_path.data, lock_path.size) == 0 &&
           memcmp(ledger_path.data + lock_path.size, suffix,
                  sizeof(suffix) - 1U) == 0;
}

int dcent_receipt_parse_binding_abi1(const void *data, size_t size,
                                     struct dcent_receipt_binding *out)
{
    struct dcent_receipt_binding parsed;
    struct parse_cursor cursor;
    struct dcent_receipt_slice value;
    uint64_t number;
    int result;

    if (out == NULL)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    memset(&parsed, 0, sizeof(parsed));
    result = cursor_init(&cursor, data, size);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_literal_line(
        &cursor, "schema=dcentos-sysupgrade-resource-ledger-abi1");
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "transaction_id", &parsed.transaction_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !id_valid(parsed.transaction_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "boot_id", &parsed.boot_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !boot_id_valid(parsed.boot_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "owner_pid", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_uint(value, INT32_MAX, &number);
    if (result != DCENT_RECEIPT_FORMAT_OK || number == 0)
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_SEMANTIC
                   : result;
    parsed.owner_pid = (uint32_t)number;
    result = take_value(&cursor, "owner_starttime", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_uint(value, UINT64_MAX, &parsed.owner_starttime);
    if (result != DCENT_RECEIPT_FORMAT_OK || parsed.owner_starttime == 0)
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_SEMANTIC
                   : result;
    result = take_value(&cursor, "owner_mount_namespace", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_devino(value, &parsed.owner_mount_namespace);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "acquisition_guard_device_inode", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_devino(value, &parsed.acquisition_guard_device_inode);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "transaction_lock_path",
                        &parsed.transaction_lock_path);
    if (result != DCENT_RECEIPT_FORMAT_OK ||
        !path_valid(parsed.transaction_lock_path) ||
        !slice_literal(parsed.transaction_lock_path,
                       "/run/dcentos-sysupgrade.lock"))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_SEMANTIC
                   : result;
    result = take_value(&cursor, "transaction_lock_device_inode", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_devino(value, &parsed.transaction_lock_device_inode);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "transaction_lock_owner_device_inode",
                        &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_devino(value,
                          &parsed.transaction_lock_owner_device_inode);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "transaction_lock_owner_sha256", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_digest(value, false,
                              &parsed.transaction_lock_owner_sha256);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "storage_mount_id", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_uint(value, UINT64_MAX, &parsed.storage_mount_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || parsed.storage_mount_id == 0U)
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_SEMANTIC
                   : result;
    result = take_value(&cursor, "ledger_path", &parsed.ledger_path);
    if (result != DCENT_RECEIPT_FORMAT_OK || !path_valid(parsed.ledger_path) ||
        !slice_literal(parsed.ledger_path,
                       "/run/dcentos-sysupgrade.lock/ledger") ||
        !binding_ledger_path_matches(parsed.transaction_lock_path,
                                     parsed.ledger_path))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_SEMANTIC
                   : result;
    result = take_value(&cursor, "ledger_device_inode", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_devino(value, &parsed.ledger_device_inode);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_literal_line(&cursor, "owner=zynq-sysupgrade");
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (cursor.offset != cursor.size)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    record_digest(data, size, parsed.record_sha256);
    *out = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_parse_lock_owner_v3(
    const void *data, size_t size, struct dcent_receipt_lock_owner *out)
{
    struct dcent_receipt_lock_owner parsed;
    struct parse_cursor cursor;
    struct dcent_receipt_slice value;
    uint64_t number;
    int result;

    if (out == NULL)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    memset(&parsed, 0, sizeof(parsed));
    result = cursor_init(&cursor, data, size);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_literal_line(&cursor, "schema=dcentos-sysupgrade-lock-v3");
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "transaction_id", &parsed.transaction_id);
    if (result != DCENT_RECEIPT_FORMAT_OK ||
        !id_valid(parsed.transaction_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "boot_id", &parsed.boot_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !boot_id_valid(parsed.boot_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "pid", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_uint(value, INT32_MAX, &number);
    if (result != DCENT_RECEIPT_FORMAT_OK || number == 0U)
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_SEMANTIC
                   : result;
    parsed.pid = (uint32_t)number;
    result = take_value(&cursor, "starttime", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_uint(value, UINT64_MAX, &parsed.starttime);
    if (result != DCENT_RECEIPT_FORMAT_OK || parsed.starttime == 0U)
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_SEMANTIC
                   : result;
    result = take_literal_line(&cursor, "owner=zynq-sysupgrade");
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (cursor.offset != cursor.size)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    record_digest(data, size, parsed.record_sha256);
    *out = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_parse_resource_intent_abi1(
    const void *data, size_t size, struct dcent_receipt_resource_intent *out)
{
    struct dcent_receipt_resource_intent parsed;
    struct parse_cursor cursor;
    struct dcent_receipt_slice value;
    int result;

    if (out == NULL)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    memset(&parsed, 0, sizeof(parsed));
    result = cursor_init(&cursor, data, size);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_literal_line(
        &cursor, "schema=dcentos-sysupgrade-resource-intent-abi1");
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "binding_sha256", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_digest(value, false, &parsed.binding_sha256);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "transaction_id", &parsed.transaction_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !id_valid(parsed.transaction_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "kind", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    parsed.kind = parse_resource_kind_slice(value);
    if (parsed.kind == DCENT_RECEIPT_KIND_INVALID)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    result = take_value(&cursor, "resource_id", &parsed.resource_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !id_valid(parsed.resource_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "provenance", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    parsed.provenance = parse_provenance_slice(value);
    if (parsed.provenance == DCENT_RECEIPT_PROVENANCE_INVALID)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    result = take_value(&cursor, "identity_a", &parsed.identity_a);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = take_value(&cursor, "identity_b", &parsed.identity_b);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = take_value(&cursor, "identity_c", &parsed.identity_c);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (!resource_identity_valid(parsed.kind, parsed.identity_a,
                                 parsed.identity_b, parsed.identity_c))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    result = parse_evidence(&cursor, &parsed.evidence);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (parsed.evidence.type !=
        resource_evidence_type(parsed.kind, DCENT_RECEIPT_RESOURCE_INVALID,
                               true))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    record_digest(data, size, parsed.record_sha256);
    *out = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

static bool resource_phase_revision_valid(
    enum dcent_receipt_resource_phase phase, uint32_t revision)
{
    switch (phase) {
    case DCENT_RECEIPT_RESOURCE_PENDING:
        return revision == 1U;
    case DCENT_RECEIPT_RESOURCE_ACTIVE:
        return revision == 2U;
    case DCENT_RECEIPT_RESOURCE_RELEASE_PENDING:
        return revision == 3U;
    case DCENT_RECEIPT_RESOURCE_RELEASED:
        return revision == 2U || revision == 4U;
    case DCENT_RECEIPT_RESOURCE_CONFLICT:
        return revision >= 2U && revision <= DCENT_RECEIPT_MAX_REVISIONS;
    case DCENT_RECEIPT_RESOURCE_INVALID:
        return false;
    }
    return false;
}

int dcent_receipt_parse_resource_status_abi1(
    const void *data, size_t size, struct dcent_receipt_resource_status *out)
{
    struct dcent_receipt_resource_status parsed;
    struct parse_cursor cursor;
    struct dcent_receipt_slice value;
    uint64_t generation;
    uint64_t revision;
    int result;

    if (out == NULL)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    memset(&parsed, 0, sizeof(parsed));
    result = cursor_init(&cursor, data, size);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_literal_line(
        &cursor, "schema=dcentos-sysupgrade-resource-status-abi1");
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "binding_sha256", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_digest(value, false, &parsed.binding_sha256);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "transaction_id", &parsed.transaction_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !id_valid(parsed.transaction_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "kind", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    parsed.kind = parse_resource_kind_slice(value);
    if (parsed.kind == DCENT_RECEIPT_KIND_INVALID)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    result = take_value(&cursor, "resource_id", &parsed.resource_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !id_valid(parsed.resource_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "intent_sha256", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_digest(value, false, &parsed.intent_sha256);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "phase", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    parsed.phase = parse_resource_phase_slice(value);
    if (parsed.phase == DCENT_RECEIPT_RESOURCE_INVALID)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    result = take_value(&cursor, "revision", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_uint(value, DCENT_RECEIPT_MAX_REVISIONS, &revision);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    parsed.revision = (uint32_t)revision;
    if (!resource_phase_revision_valid(parsed.phase, parsed.revision))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    result = take_value(&cursor, "ledger_generation", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_uint(value, DCENT_RECEIPT_MAX_LEDGER_GENERATION,
                            &generation);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (generation == 0U)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    parsed.ledger_generation = (uint32_t)generation;
    result = take_value(&cursor, "previous_status_sha256", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_digest(value, true, &parsed.previous_status_sha256);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "actor_kind", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    parsed.actor_kind = parse_actor_kind_slice(value);
    if (parsed.actor_kind == DCENT_RECEIPT_ACTOR_INVALID)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    result = take_value(&cursor, "actor_id", &parsed.actor_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !id_valid(parsed.actor_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = parse_evidence(&cursor, &parsed.evidence);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (parsed.phase == DCENT_RECEIPT_RESOURCE_PENDING) {
        if (parsed.revision != 1U || parsed.previous_status_sha256.present ||
            parsed.actor_kind != DCENT_RECEIPT_ACTOR_OWNER ||
            parsed.evidence.type != DCENT_RECEIPT_EVIDENCE_NONE)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
    } else {
        if (!parsed.previous_status_sha256.present ||
            parsed.evidence.type !=
                resource_evidence_type(parsed.kind, parsed.phase, false))
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
    }
    record_digest(data, size, parsed.record_sha256);
    *out = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_parse_claim_intent_abi1(
    const void *data, size_t size, struct dcent_receipt_claim_intent *out)
{
    struct dcent_receipt_claim_intent parsed;
    struct parse_cursor cursor;
    struct dcent_receipt_slice value;
    uint64_t number;
    int result;

    if (out == NULL)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    memset(&parsed, 0, sizeof(parsed));
    result = cursor_init(&cursor, data, size);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_literal_line(
        &cursor, "schema=dcentos-sysupgrade-reconcile-intent-abi1");
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "binding_sha256", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_digest(value, false, &parsed.binding_sha256);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "transaction_id", &parsed.transaction_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !id_valid(parsed.transaction_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "claim_id", &parsed.claim_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !id_valid(parsed.claim_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "reconciler_boot_id",
                        &parsed.reconciler_boot_id);
    if (result != DCENT_RECEIPT_FORMAT_OK ||
        !boot_id_valid(parsed.reconciler_boot_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "reconciler_pid", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_uint(value, INT32_MAX, &number);
    if (result != DCENT_RECEIPT_FORMAT_OK || number == 0)
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_SEMANTIC
                   : result;
    parsed.reconciler_pid = (uint32_t)number;
    result = take_value(&cursor, "reconciler_starttime", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_uint(value, UINT64_MAX, &parsed.reconciler_starttime);
    if (result != DCENT_RECEIPT_FORMAT_OK ||
        parsed.reconciler_starttime == 0)
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_SEMANTIC
                   : result;
    result = take_value(&cursor, "reconciler_mount_namespace", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_devino(value, &parsed.reconciler_mount_namespace);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "maintenance_lock_path",
                        &parsed.maintenance_lock_path);
    if (result != DCENT_RECEIPT_FORMAT_OK ||
        !path_valid(parsed.maintenance_lock_path))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "maintenance_lock_device_inode", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_devino(value, &parsed.maintenance_lock_device_inode);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_literal_line(&cursor, "owner=zynq-sysupgrade-reconciler");
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_evidence(&cursor, &parsed.evidence);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (parsed.evidence.type != DCENT_RECEIPT_EVIDENCE_OWNER_DEATH)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    record_digest(data, size, parsed.record_sha256);
    *out = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

static bool claim_phase_revision_valid(enum dcent_receipt_claim_phase phase,
                                       uint32_t revision)
{
    switch (phase) {
    case DCENT_RECEIPT_CLAIM_CLAIMED:
        return revision == 1U;
    case DCENT_RECEIPT_CLAIM_QUIESCENT:
        return revision == 2U;
    case DCENT_RECEIPT_CLAIM_RECONCILING:
        return revision == 3U;
    case DCENT_RECEIPT_CLAIM_COMPLETE:
        return revision == 4U;
    case DCENT_RECEIPT_CLAIM_BLOCKED:
        return revision >= 2U && revision <= DCENT_RECEIPT_MAX_REVISIONS;
    case DCENT_RECEIPT_CLAIM_INVALID:
        return false;
    }
    return false;
}

int dcent_receipt_parse_claim_status_abi1(
    const void *data, size_t size, struct dcent_receipt_claim_status *out)
{
    struct dcent_receipt_claim_status parsed;
    struct parse_cursor cursor;
    struct dcent_receipt_slice value;
    uint64_t generation;
    uint64_t revision;
    int result;

    if (out == NULL)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    memset(&parsed, 0, sizeof(parsed));
    result = cursor_init(&cursor, data, size);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_literal_line(
        &cursor, "schema=dcentos-sysupgrade-reconcile-status-abi1");
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "claim_intent_sha256", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_digest(value, false, &parsed.claim_intent_sha256);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "phase", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    parsed.phase = parse_claim_phase_slice(value);
    if (parsed.phase == DCENT_RECEIPT_CLAIM_INVALID)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    result = take_value(&cursor, "revision", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_uint(value, DCENT_RECEIPT_MAX_REVISIONS, &revision);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    parsed.revision = (uint32_t)revision;
    if (!claim_phase_revision_valid(parsed.phase, parsed.revision))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    result = take_value(&cursor, "ledger_generation", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_uint(value, DCENT_RECEIPT_MAX_LEDGER_GENERATION,
                            &generation);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (generation == 0U)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    parsed.ledger_generation = (uint32_t)generation;
    result = take_value(&cursor, "previous_status_sha256", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_digest(value, true, &parsed.previous_status_sha256);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "actor_id", &parsed.actor_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !id_valid(parsed.actor_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "quiescence_sha256", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_digest(value, true, &parsed.quiescence_sha256);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = parse_evidence(&cursor, &parsed.evidence);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if ((parsed.revision == 1U) != !parsed.previous_status_sha256.present)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    switch (parsed.phase) {
    case DCENT_RECEIPT_CLAIM_CLAIMED:
        if (parsed.quiescence_sha256.present ||
            parsed.evidence.type != DCENT_RECEIPT_EVIDENCE_NONE)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        break;
    case DCENT_RECEIPT_CLAIM_QUIESCENT:
        if (!parsed.quiescence_sha256.present ||
            parsed.evidence.type !=
                DCENT_RECEIPT_EVIDENCE_MAINTENANCE_QUIESCENCE ||
            !digest_equal(parsed.quiescence_sha256.bytes,
                          parsed.evidence.digest.bytes))
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        break;
    case DCENT_RECEIPT_CLAIM_RECONCILING:
        if (!parsed.quiescence_sha256.present ||
            parsed.evidence.type !=
                DCENT_RECEIPT_EVIDENCE_RECONCILIATION_BEGIN)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        break;
    case DCENT_RECEIPT_CLAIM_COMPLETE:
        if (!parsed.quiescence_sha256.present ||
            parsed.evidence.type !=
                DCENT_RECEIPT_EVIDENCE_RECONCILIATION_COMPLETE)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        break;
    case DCENT_RECEIPT_CLAIM_BLOCKED:
        if ((parsed.revision == 2U) == parsed.quiescence_sha256.present ||
            parsed.evidence.type !=
                DCENT_RECEIPT_EVIDENCE_RECONCILIATION_BLOCKED)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        break;
    case DCENT_RECEIPT_CLAIM_INVALID:
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    }
    record_digest(data, size, parsed.record_sha256);
    *out = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_parse_transaction_phase_status_abi2(
    const void *data, size_t size,
    struct dcent_receipt_transaction_phase_status *out)
{
    struct dcent_receipt_transaction_phase_status parsed;
    struct parse_cursor cursor;
    struct dcent_receipt_slice value;
    uint64_t generation;
    uint64_t revision;
    int result;

    if (out == NULL)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    memset(&parsed, 0, sizeof(parsed));
    result = cursor_init(&cursor, data, size);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_literal_line(
        &cursor,
        "schema=dcentos-sysupgrade-transaction-phase-status-abi2");
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "binding_sha256", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_digest(value, false, &parsed.binding_sha256);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "transaction_id", &parsed.transaction_id);
    if (result != DCENT_RECEIPT_FORMAT_OK ||
        !id_valid(parsed.transaction_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = take_value(&cursor, "phase", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    parsed.phase = parse_lock_phase_slice(value);
    if (parsed.phase == DCENT_RECEIPT_LOCK_INVALID)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    result = take_value(&cursor, "revision", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_uint(value, DCENT_RECEIPT_MAX_PHASE_REVISIONS,
                            &revision);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (revision == 0U)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    parsed.revision = (uint32_t)revision;
    result = take_value(&cursor, "ledger_generation", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_uint(value, DCENT_RECEIPT_MAX_LEDGER_GENERATION,
                            &generation);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if (generation == 0U)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    parsed.ledger_generation = (uint32_t)generation;
    result = take_value(&cursor, "previous_status_sha256", &value);
    if (result == DCENT_RECEIPT_FORMAT_OK)
        result = parse_digest(value, true, &parsed.previous_status_sha256);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    result = take_value(&cursor, "actor_kind", &value);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    parsed.actor_kind = parse_actor_kind_slice(value);
    if (parsed.actor_kind == DCENT_RECEIPT_ACTOR_INVALID)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    result = take_value(&cursor, "actor_id", &parsed.actor_id);
    if (result != DCENT_RECEIPT_FORMAT_OK || !id_valid(parsed.actor_id))
        return result == DCENT_RECEIPT_FORMAT_OK
                   ? DCENT_RECEIPT_FORMAT_MALFORMED
                   : result;
    result = parse_evidence(&cursor, &parsed.evidence);
    if (result != DCENT_RECEIPT_FORMAT_OK)
        return result;
    if ((parsed.revision == 1U) != !parsed.previous_status_sha256.present ||
        parsed.evidence.type != transaction_phase_evidence_type(parsed.phase))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    if (parsed.phase != DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED &&
        parsed.actor_kind != DCENT_RECEIPT_ACTOR_OWNER)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    record_digest(data, size, parsed.record_sha256);
    *out = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_binding_anchor_init(
    struct dcent_receipt_binding_anchor *anchor,
    const struct dcent_receipt_binding *binding)
{
    struct dcent_receipt_binding_anchor parsed;

    if (anchor == NULL || binding == NULL ||
        !id_valid(binding->transaction_id) ||
        !boot_id_valid(binding->boot_id) || binding->owner_pid == 0U ||
        binding->owner_starttime == 0U ||
        !digest_present(&binding->transaction_lock_owner_sha256) ||
        binding->storage_mount_id == 0U ||
        !path_valid(binding->transaction_lock_path) ||
        !slice_literal(binding->transaction_lock_path,
                       "/run/dcentos-sysupgrade.lock") ||
        !path_valid(binding->ledger_path) ||
        !slice_literal(binding->ledger_path,
                       "/run/dcentos-sysupgrade.lock/ledger") ||
        !binding_ledger_path_matches(binding->transaction_lock_path,
                                     binding->ledger_path))
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    memset(&parsed, 0, sizeof(parsed));
    parsed.initialized = true;
    if (!id_copy(&parsed.transaction_id, binding->transaction_id) ||
        !id_copy(&parsed.boot_id, binding->boot_id) ||
        !path_copy(&parsed.transaction_lock_path,
                   binding->transaction_lock_path) ||
        !path_copy(&parsed.ledger_path, binding->ledger_path))
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    parsed.owner_pid = binding->owner_pid;
    parsed.owner_starttime = binding->owner_starttime;
    parsed.owner_mount_namespace = binding->owner_mount_namespace;
    parsed.acquisition_guard_device_inode =
        binding->acquisition_guard_device_inode;
    parsed.transaction_lock_device_inode =
        binding->transaction_lock_device_inode;
    parsed.transaction_lock_owner_device_inode =
        binding->transaction_lock_owner_device_inode;
    parsed.transaction_lock_owner_sha256 =
        binding->transaction_lock_owner_sha256;
    parsed.storage_mount_id = binding->storage_mount_id;
    parsed.ledger_device_inode = binding->ledger_device_inode;
    memcpy(parsed.record_sha256, binding->record_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    *anchor = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_lock_anchor_init(
    struct dcent_receipt_lock_anchor *anchor,
    const struct dcent_receipt_lock_owner *owner)
{
    struct dcent_receipt_lock_anchor parsed;

    if (anchor == NULL || owner == NULL ||
        !id_valid(owner->transaction_id) || !boot_id_valid(owner->boot_id) ||
        owner->pid == 0U || owner->starttime == 0U)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    memset(&parsed, 0, sizeof(parsed));
    parsed.initialized = true;
    if (!id_copy(&parsed.transaction_id, owner->transaction_id) ||
        !id_copy(&parsed.boot_id, owner->boot_id))
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    parsed.pid = owner->pid;
    parsed.starttime = owner->starttime;
    memcpy(parsed.record_sha256, owner->record_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    *anchor = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_resource_chain_begin(
    struct dcent_receipt_resource_chain *chain,
    const struct dcent_receipt_binding_anchor *binding,
    const struct dcent_receipt_resource_intent *intent)
{
    struct dcent_receipt_resource_chain parsed;

    if (chain == NULL || binding == NULL || !binding->initialized ||
        intent == NULL ||
        intent->kind == DCENT_RECEIPT_KIND_INVALID ||
        !digest_present(&intent->binding_sha256) ||
        !owned_id_valid(&binding->transaction_id) ||
        !id_valid(intent->transaction_id) || !id_valid(intent->resource_id))
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    if (!id_equal_slice(&binding->transaction_id, intent->transaction_id) ||
        !digest_equal(intent->binding_sha256.bytes, binding->record_sha256))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    memset(&parsed, 0, sizeof(parsed));
    parsed.initialized = true;
    parsed.kind = intent->kind;
    if (!id_copy(&parsed.transaction_id, intent->transaction_id) ||
        !id_copy(&parsed.resource_id, intent->resource_id))
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    parsed.binding_sha256 = intent->binding_sha256;
    parsed.authority = DCENT_RECEIPT_AUTHORITY_OWNER;
    memcpy(parsed.intent_sha256, intent->record_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    *chain = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_resource_chain_add(
    struct dcent_receipt_resource_chain *chain,
    const struct dcent_receipt_resource_status *status)
{
    struct dcent_receipt_resource_chain next;

    if (chain == NULL || status == NULL || !chain->initialized)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    if (!id_valid(status->transaction_id) ||
        !id_valid(status->resource_id) || !id_valid(status->actor_id) ||
        !digest_present(&status->binding_sha256) ||
        !digest_present(&status->intent_sha256) ||
        status->ledger_generation == 0U ||
        status->ledger_generation > DCENT_RECEIPT_MAX_LEDGER_GENERATION)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    next = *chain;
    if (chain->revisions >= DCENT_RECEIPT_MAX_REVISIONS ||
        status->revision != chain->revisions + 1U)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    if (status->kind != chain->kind ||
        !id_equal_slice(&chain->transaction_id, status->transaction_id) ||
        !id_equal_slice(&chain->resource_id, status->resource_id) ||
        !digest_equal(status->binding_sha256.bytes,
                      chain->binding_sha256.bytes) ||
        !digest_equal(status->intent_sha256.bytes, chain->intent_sha256))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;

    if (chain->revisions == 0U) {
        if (status->phase != DCENT_RECEIPT_RESOURCE_PENDING ||
            status->previous_status_sha256.present ||
            status->actor_kind != DCENT_RECEIPT_ACTOR_OWNER ||
            !id_equal_slice(&chain->transaction_id, status->actor_id))
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
    } else {
        if (status->ledger_generation <= chain->latest_ledger_generation ||
            !status->previous_status_sha256.present ||
            !digest_equal(status->previous_status_sha256.bytes,
                          chain->latest_status_sha256) ||
            !dcent_receipt_resource_transition_valid(chain->latest_phase,
                                                     status->phase))
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        if (status->actor_kind == DCENT_RECEIPT_ACTOR_OWNER) {
            if (chain->authority != DCENT_RECEIPT_AUTHORITY_OWNER ||
                !id_equal_slice(&chain->transaction_id, status->actor_id))
                return DCENT_RECEIPT_FORMAT_SEMANTIC;
        } else if (status->actor_kind == DCENT_RECEIPT_ACTOR_RECONCILER) {
            if (chain->authority == DCENT_RECEIPT_AUTHORITY_RECONCILER &&
                !id_equal_slice(&chain->reconciler_id, status->actor_id))
                return DCENT_RECEIPT_FORMAT_SEMANTIC;
            next.authority = DCENT_RECEIPT_AUTHORITY_RECONCILER;
            if (!id_copy(&next.reconciler_id, status->actor_id))
                return DCENT_RECEIPT_FORMAT_MALFORMED;
        } else {
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        }
    }
    next.latest_phase = status->phase;
    next.revisions = status->revision;
    next.latest_ledger_generation = status->ledger_generation;
    memcpy(next.latest_status_sha256, status->record_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    *chain = next;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_resource_chain_finish(
    const struct dcent_receipt_resource_chain *chain)
{
    if (chain == NULL || !chain->initialized || chain->revisions == 0U ||
        chain->revisions > DCENT_RECEIPT_MAX_REVISIONS ||
        !owned_id_valid(&chain->transaction_id) ||
        !owned_id_valid(&chain->resource_id) ||
        !digest_present(&chain->binding_sha256) ||
        chain->latest_ledger_generation == 0U ||
        chain->latest_ledger_generation > DCENT_RECEIPT_MAX_LEDGER_GENERATION ||
        chain->latest_phase < DCENT_RECEIPT_RESOURCE_PENDING ||
        chain->latest_phase > DCENT_RECEIPT_RESOURCE_CONFLICT ||
        (chain->authority != DCENT_RECEIPT_AUTHORITY_OWNER &&
         chain->authority != DCENT_RECEIPT_AUTHORITY_RECONCILER) ||
        (chain->authority == DCENT_RECEIPT_AUTHORITY_RECONCILER &&
         !owned_id_valid(&chain->reconciler_id)))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_claim_chain_begin(
    struct dcent_receipt_claim_chain *chain,
    const struct dcent_receipt_binding_anchor *binding,
    const struct dcent_receipt_claim_intent *intent)
{
    struct dcent_receipt_claim_chain parsed;

    if (chain == NULL || binding == NULL || !binding->initialized ||
        intent == NULL ||
        !digest_present(&intent->binding_sha256) ||
        !owned_id_valid(&binding->transaction_id) ||
        !id_valid(intent->transaction_id) || !id_valid(intent->claim_id) ||
        !boot_id_valid(intent->reconciler_boot_id) ||
        intent->reconciler_pid == 0U || intent->reconciler_starttime == 0U ||
        !path_valid(intent->maintenance_lock_path) ||
        intent->evidence.type != DCENT_RECEIPT_EVIDENCE_OWNER_DEATH ||
        !digest_present(&intent->evidence.digest))
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    if (!id_equal_slice(&binding->transaction_id, intent->transaction_id) ||
        !id_equal_slice(&binding->boot_id, intent->reconciler_boot_id) ||
        !digest_equal(intent->binding_sha256.bytes, binding->record_sha256))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    memset(&parsed, 0, sizeof(parsed));
    parsed.initialized = true;
    if (!id_copy(&parsed.transaction_id, intent->transaction_id) ||
        !id_copy(&parsed.claim_id, intent->claim_id) ||
        !id_copy(&parsed.reconciler_boot_id, intent->reconciler_boot_id) ||
        !path_copy(&parsed.maintenance_lock_path,
                   intent->maintenance_lock_path))
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    parsed.reconciler_pid = intent->reconciler_pid;
    parsed.reconciler_starttime = intent->reconciler_starttime;
    parsed.reconciler_mount_namespace = intent->reconciler_mount_namespace;
    parsed.maintenance_lock_device_inode =
        intent->maintenance_lock_device_inode;
    parsed.owner_death_evidence_sha256 = intent->evidence.digest;
    parsed.binding_sha256 = intent->binding_sha256;
    memcpy(parsed.intent_sha256, intent->record_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    *chain = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_claim_chain_add(
    struct dcent_receipt_claim_chain *chain,
    const struct dcent_receipt_claim_status *status)
{
    struct dcent_receipt_claim_chain next;

    if (chain == NULL || status == NULL || !chain->initialized)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    if (!id_valid(status->actor_id) ||
        !digest_present(&status->claim_intent_sha256) ||
        status->ledger_generation == 0U ||
        status->ledger_generation > DCENT_RECEIPT_MAX_LEDGER_GENERATION)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    next = *chain;
    if (chain->revisions >= DCENT_RECEIPT_MAX_REVISIONS ||
        status->revision != chain->revisions + 1U ||
        !digest_equal(status->claim_intent_sha256.bytes,
                      chain->intent_sha256) ||
        !id_equal_slice(&chain->claim_id, status->actor_id))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    if (chain->revisions == 0U) {
        if (status->phase != DCENT_RECEIPT_CLAIM_CLAIMED ||
            status->previous_status_sha256.present)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
    } else {
        if (status->ledger_generation <= chain->latest_ledger_generation ||
            !status->previous_status_sha256.present ||
            !digest_equal(status->previous_status_sha256.bytes,
                          chain->latest_status_sha256) ||
            !dcent_receipt_claim_transition_valid(chain->latest_phase,
                                                  status->phase))
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        if (status->phase == DCENT_RECEIPT_CLAIM_QUIESCENT) {
            next.quiescence_sha256 = status->quiescence_sha256;
        } else if (chain->quiescence_sha256.present !=
                       status->quiescence_sha256.present ||
                   (chain->quiescence_sha256.present &&
                    !digest_equal(chain->quiescence_sha256.bytes,
                                  status->quiescence_sha256.bytes))) {
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        }
    }
    if (status->phase == DCENT_RECEIPT_CLAIM_RECONCILING)
        next.saw_reconciling = true;
    next.latest_phase = status->phase;
    next.revisions = status->revision;
    next.latest_ledger_generation = status->ledger_generation;
    memcpy(next.latest_status_sha256, status->record_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    *chain = next;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_claim_chain_finish(
    const struct dcent_receipt_claim_chain *chain)
{
    if (chain == NULL || !chain->initialized || chain->revisions == 0U ||
        chain->revisions > DCENT_RECEIPT_MAX_REVISIONS ||
        !owned_id_valid(&chain->transaction_id) ||
        !owned_id_valid(&chain->claim_id) ||
        !owned_boot_id_valid(&chain->reconciler_boot_id) ||
        chain->reconciler_pid == 0U || chain->reconciler_starttime == 0U ||
        !owned_path_valid(&chain->maintenance_lock_path) ||
        !digest_present(&chain->owner_death_evidence_sha256) ||
        !digest_present(&chain->binding_sha256) ||
        chain->latest_ledger_generation == 0U ||
        chain->latest_ledger_generation > DCENT_RECEIPT_MAX_LEDGER_GENERATION ||
        chain->latest_phase < DCENT_RECEIPT_CLAIM_CLAIMED ||
        chain->latest_phase > DCENT_RECEIPT_CLAIM_BLOCKED)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_transaction_phase_chain_begin(
    struct dcent_receipt_transaction_phase_chain *chain,
    const struct dcent_receipt_binding_anchor *binding)
{
    struct dcent_receipt_transaction_phase_chain parsed;

    if (chain == NULL || binding == NULL || !binding->initialized ||
        !owned_id_valid(&binding->transaction_id))
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    memset(&parsed, 0, sizeof(parsed));
    parsed.initialized = true;
    parsed.latest_phase = DCENT_RECEIPT_LOCK_ACTIVE;
    parsed.transaction_id = binding->transaction_id;
    parsed.binding_sha256.present = true;
    memcpy(parsed.binding_sha256.bytes, binding->record_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    *chain = parsed;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_transaction_phase_chain_add(
    struct dcent_receipt_transaction_phase_chain *chain,
    const struct dcent_receipt_transaction_phase_status *status)
{
    struct dcent_receipt_transaction_phase_chain next;

    if (chain == NULL || status == NULL || !chain->initialized ||
        !owned_id_valid(&chain->transaction_id) ||
        !digest_present(&chain->binding_sha256) ||
        !digest_present(&status->binding_sha256) ||
        !id_valid(status->transaction_id) || !id_valid(status->actor_id) ||
        status->ledger_generation == 0U ||
        status->ledger_generation > DCENT_RECEIPT_MAX_LEDGER_GENERATION)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    if (chain->revisions >= DCENT_RECEIPT_MAX_PHASE_REVISIONS ||
        status->revision != chain->revisions + 1U ||
        status->ledger_generation <= chain->latest_ledger_generation ||
        !id_equal_slice(&chain->transaction_id, status->transaction_id) ||
        !digest_equal(chain->binding_sha256.bytes,
                      status->binding_sha256.bytes) ||
        !transaction_phase_transition_valid(chain->latest_phase,
                                            status->phase))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    if (chain->revisions == 0U) {
        if (status->previous_status_sha256.present)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
    } else if (!status->previous_status_sha256.present ||
               !digest_equal(status->previous_status_sha256.bytes,
                             chain->latest_status_sha256)) {
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    }
    if (status->actor_kind == DCENT_RECEIPT_ACTOR_OWNER) {
        if (!id_equal_slice(&chain->transaction_id, status->actor_id))
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
    } else if (status->actor_kind != DCENT_RECEIPT_ACTOR_RECONCILER ||
               status->phase != DCENT_RECEIPT_LOCK_CLEANUP_REQUIRED) {
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    }
    next = *chain;
    next.latest_phase = status->phase;
    next.revisions = status->revision;
    next.latest_ledger_generation = status->ledger_generation;
    memcpy(next.latest_status_sha256, status->record_sha256,
           DCENT_RECEIPT_SHA256_BYTES);
    *chain = next;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_transaction_phase_chain_finish(
    const struct dcent_receipt_transaction_phase_chain *chain)
{
    if (chain == NULL || !chain->initialized ||
        !owned_id_valid(&chain->transaction_id) ||
        !digest_present(&chain->binding_sha256) ||
        chain->revisions > DCENT_RECEIPT_MAX_PHASE_REVISIONS ||
        chain->latest_phase < DCENT_RECEIPT_LOCK_ACTIVE ||
        chain->latest_phase > DCENT_RECEIPT_LOCK_ENV_COMMITTED ||
        ((chain->revisions == 0U) !=
         (chain->latest_ledger_generation == 0U)))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    return DCENT_RECEIPT_FORMAT_OK;
}

int dcent_receipt_ledger_validate_summary(
    const struct dcent_receipt_binding_anchor *binding,
    const struct dcent_receipt_resource_chain *resources, size_t resource_count,
    const struct dcent_receipt_claim_chain *claim, size_t aggregate_bytes)
{
    size_t left;

    if (binding == NULL || !binding->initialized ||
        !owned_id_valid(&binding->transaction_id))
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    if (resource_count > DCENT_RECEIPT_MAX_RESOURCES ||
        aggregate_bytes > DCENT_RECEIPT_MAX_LEDGER)
        return DCENT_RECEIPT_FORMAT_LIMIT;
    if (resource_count != 0U && resources == NULL)
        return DCENT_RECEIPT_FORMAT_MALFORMED;
    if (claim != NULL &&
        dcent_receipt_claim_chain_finish(claim) != DCENT_RECEIPT_FORMAT_OK)
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    for (left = 0; left < resource_count; ++left) {
        size_t right;

        if (dcent_receipt_resource_chain_finish(&resources[left]) !=
            DCENT_RECEIPT_FORMAT_OK)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        if (!id_equal(&resources[left].transaction_id,
                      &binding->transaction_id) ||
            !digest_equal(resources[left].binding_sha256.bytes,
                          binding->record_sha256))
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        for (right = left + 1U; right < resource_count; ++right) {
            if (resources[left].kind == resources[right].kind &&
                id_equal(&resources[left].resource_id,
                         &resources[right].resource_id))
                return DCENT_RECEIPT_FORMAT_SEMANTIC;
        }
        if (claim != NULL &&
            (!id_equal(&resources[left].transaction_id,
                       &claim->transaction_id) ||
             !digest_equal(resources[left].binding_sha256.bytes,
                           claim->binding_sha256.bytes)))
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        if (resources[left].authority == DCENT_RECEIPT_AUTHORITY_RECONCILER &&
            (claim == NULL || !claim->saw_reconciling ||
             !id_equal(&resources[left].reconciler_id, &claim->claim_id)))
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
        if (claim != NULL &&
            claim->latest_phase == DCENT_RECEIPT_CLAIM_COMPLETE &&
            resources[left].latest_phase != DCENT_RECEIPT_RESOURCE_RELEASED)
            return DCENT_RECEIPT_FORMAT_SEMANTIC;
    }
    if (claim != NULL &&
        (!id_equal(&claim->transaction_id, &binding->transaction_id) ||
         !digest_equal(claim->binding_sha256.bytes,
                       binding->record_sha256)))
        return DCENT_RECEIPT_FORMAT_SEMANTIC;
    return DCENT_RECEIPT_FORMAT_OK;
}
