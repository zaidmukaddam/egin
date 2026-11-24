import unittest

from pricing import total


class TestPricing(unittest.TestCase):
    def test_small_order(self):
        self.assertEqual(total(2.50, 4), 10.00)

    def test_bulk_discount(self):
        self.assertEqual(total(10.00, 10), 90.00)

    def test_large_order(self):
        self.assertEqual(total(1.99, 100), 179.10)


if __name__ == "__main__":
    unittest.main()
