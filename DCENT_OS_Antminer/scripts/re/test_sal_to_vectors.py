import io
import struct
import unittest

import sal_to_vectors


class SaleaeVectorTests(unittest.TestCase):
    def test_documented_v1_stream_and_uart_decode(self):
        baud = 1_000
        bits = [0] + [(0xA5 >> bit) & 1 for bit in range(8)] + [1]
        state = 1
        transitions = []
        for index, bit in enumerate(bits):
            if bit != state:
                transitions.append(index / baud)
                state = bit
        data = bytearray(b"<SALEAE>")
        data.extend(struct.pack("<iiQ", 1, 0, 1))
        data.extend(struct.pack("<IdddQ", 1, 1_000_000.0, 0.0, 0.02, len(transitions)))
        data.extend(struct.pack(f"<{len(transitions)}d", *transitions))
        chunks = sal_to_vectors.parse_saleae_digital(io.BytesIO(data))
        self.assertEqual(sal_to_vectors.decode_uart_8n1(chunks, baud), [(0.0, 0xA5)])

    def test_private_archive_stream_fails_explicitly(self):
        data = io.BytesIO(b"<SALEAE>" + struct.pack("<ii", 1, 100))
        with self.assertRaisesRegex(ValueError, "private .sal"):
            sal_to_vectors.parse_saleae_digital(data)


if __name__ == "__main__":
    unittest.main()
