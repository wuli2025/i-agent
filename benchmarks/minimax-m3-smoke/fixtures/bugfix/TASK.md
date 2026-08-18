# 任务：修复库存汇总函数

请修复 `inventory.py` 中的 `summarize(records)`，并运行测试。不得修改函数签名，也不得添加第三方依赖。

规则：

1. `records` 是字典序列；每条必须包含 `sku`、`qty`、`unit_price`，`discount` 可省略或为空（视为 0）。
2. SKU 必须去掉首尾空白并转成大写；空 SKU 要抛出 `ValueError`。
3. `qty` 用 `int` 解析，可为负数（退货）；`unit_price`、`discount` 必须用 `decimal.Decimal` 计算，禁止经过 `float`。
4. `unit_price` 必须大于 0；`discount` 必须位于闭区间 `[0, 1]`，否则抛出 `ValueError`。
5. 同一标准化 SKU 要汇总数量与净额：`qty * unit_price * (1 - discount)`。
6. 净额必须**先精确汇总、最后一次性**按 `ROUND_HALF_UP` 舍入到 2 位小数，输出为固定两位的小数字符串。
7. 返回普通字典，键按 SKU 升序插入；值形状严格为 `{"qty": int, "net": "0.00"}`。
8. 保留并通过现有测试；可以补充测试，但不要删改原有断言。

完成前执行：

```text
python -m unittest -v
```
