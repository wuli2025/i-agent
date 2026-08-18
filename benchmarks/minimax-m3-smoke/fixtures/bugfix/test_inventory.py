import unittest

from inventory import summarize


class SummarizeTests(unittest.TestCase):
    def test_normalizes_and_aggregates_sku(self):
        rows = [
            {"sku": " pen ", "qty": "2", "unit_price": "1.25", "discount": "0"},
            {"sku": "PEN", "qty": "3", "unit_price": "1.25", "discount": "0.20"},
        ]
        self.assertEqual(summarize(rows), {"PEN": {"qty": 5, "net": "5.50"}})

    def test_rejects_discount_above_one(self):
        with self.assertRaises(ValueError):
            summarize([{"sku": "A", "qty": 1, "unit_price": "5", "discount": "1.01"}])


if __name__ == "__main__":
    unittest.main()
