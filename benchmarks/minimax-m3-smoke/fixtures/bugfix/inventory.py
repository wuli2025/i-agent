"""Inventory aggregation exercise.

The implementation is intentionally incomplete. Follow TASK.md (provided by the
benchmark runner) and keep the public function signature unchanged.
"""


def summarize(records):
    result = {}
    for row in records:
        sku = row["sku"]
        qty = int(row["qty"])
        unit_price = float(row["unit_price"])
        discount = float(row.get("discount") or 0)
        net = round(qty * unit_price * (1 - discount), 2)

        if sku not in result:
            result[sku] = {"qty": qty, "net": f"{net:.2f}"}
        else:
            result[sku]["qty"] = qty
            result[sku]["net"] = f"{float(result[sku]['net']) + net:.2f}"
    return result
