"""Pure Python wire primitives shared by DCENT_OS diagnostic tools.

Host-command and ASIC-response CRCs are separate protocols.  Callers must
select the direction-specific primitive; neither is a generic BM13xx CRC.
"""

CRC5_BM13XX_COMMAND_POLY = 0x05
CRC5_BM13XX_COMMAND_INIT = 0x1F
CRC5_BM13XX_COMMAND_MASK = 0x1F
CRC5_BM13XX_RESPONSE_COMMAND_INIT = 0x03
CRC5_BM13XX_RESPONSE_JOB_INIT = 0x1B
CRC5_BM13XX_RESPONSE_MASK = 0x1F
BM1387_CAPTURED_PROTOCOL_PROFILE = None

__all__ = [
    "CRC5_BM13XX_COMMAND_POLY",
    "CRC5_BM13XX_COMMAND_INIT",
    "CRC5_BM13XX_COMMAND_MASK",
    "CRC5_BM13XX_RESPONSE_COMMAND_INIT",
    "CRC5_BM13XX_RESPONSE_JOB_INIT",
    "CRC5_BM13XX_RESPONSE_MASK",
    "BM1387_CAPTURED_PROTOCOL_PROFILE",
    "crc5_bm13xx_command",
    "crc5_bm13xx_response",
    "require_captured_bm1387_protocol_profile",
]


def require_captured_bm1387_protocol_profile():
    """Refuse hardware transmission until a capture-backed profile exists."""

    raise RuntimeError(
        "BM1387 FIL/VIL command framing is not capture-validated; "
        "hardware transmission is disabled"
    )


def crc5_bm13xx_command(data, *, bit_length=None):
    """Return the BM13xx host-command CRC5 for ``data``.

    The LFSR is width 5, polynomial 0x05, initial value 0x1f, MSB first,
    without reflection or a final XOR.  ``bit_length`` defaults to every bit
    in ``data`` and exists for legacy FIL frames whose CRC field shares the
    final byte.  It is deliberately keyword-only so byte lengths cannot be
    confused with bit lengths.
    """

    if not isinstance(data, (bytes, bytearray, memoryview)):
        raise TypeError("data must be bytes-like")
    payload = bytes(data)

    available_bits = len(payload) * 8
    if bit_length is None:
        bit_length = available_bits
    elif isinstance(bit_length, bool) or not isinstance(bit_length, int):
        raise TypeError("bit_length must be an integer or None")

    if bit_length < 0 or bit_length > available_bits:
        raise ValueError(
            "bit_length must be between 0 and {} (got {})".format(
                available_bits, bit_length
            )
        )

    crc = CRC5_BM13XX_COMMAND_INIT
    for bit_index in range(bit_length):
        byte = payload[bit_index // 8]
        data_bit = (byte >> (7 - (bit_index % 8))) & 1
        feedback = ((crc >> 4) & 1) ^ data_bit
        crc = (crc << 1) & CRC5_BM13XX_COMMAND_MASK
        if feedback:
            crc ^= CRC5_BM13XX_COMMAND_POLY

    return crc & CRC5_BM13XX_COMMAND_MASK


def crc5_bm13xx_response(data, *, is_job_response):
    """Return the response-specific BM13xx CRC5 for ``data``.

    This implements Braiins ``crc5_resp_serial`` exactly.  It is not the
    host-command polynomial: the response state transition is modified 0x0d,
    and the initial state is 0x03 for command/register replies or 0x1b for job
    replies.  ``data`` excludes the ``aa55`` preamble and final flags/CRC byte.

    The caller must classify the final trailer's bit 7 and pass that explicit
    boolean here.  Bits 6:5 are not interpreted by this primitive.
    """

    if not isinstance(data, (bytes, bytearray, memoryview)):
        raise TypeError("data must be bytes-like")
    if not isinstance(is_job_response, bool):
        raise TypeError("is_job_response must be a boolean")

    crc = (
        CRC5_BM13XX_RESPONSE_JOB_INIT
        if is_job_response
        else CRC5_BM13XX_RESPONSE_COMMAND_INIT
    )
    for byte in bytes(data):
        for bit_index in range(7, -1, -1):
            data_bit = (byte >> bit_index) & 1
            previous = crc
            feedback = ((previous >> 4) & 1) ^ data_bit

            crc = ((previous & 0x0F) << 1) | feedback
            crc = (crc & ~(1 << 2)) | ((((previous >> 1) & 1) ^ feedback) << 2)
            crc = (crc & ~(1 << 3)) | ((((previous >> 2) & 1) ^ data_bit) << 3)
            crc &= CRC5_BM13XX_RESPONSE_MASK

    return crc
