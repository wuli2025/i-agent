# 任务：清洗订单并生成办公报告

只使用 Python 标准库处理 `orders.csv`，生成以下三个 UTF-8 文件：

- `cleaned_orders.csv`
- `rejected_orders.csv`
- `summary.json`
- `report.md`

## 清洗规则

1. 所有文本字段先去掉首尾空白；`status` 转成小写。
2. `order_id` 不得为空；`order_date` 必须是真实存在的 `YYYY-MM-DD` 日期；`quantity` 必须是正整数；`unit_price` 必须是大于 0 的十进制数；`status` 只能是 `paid`、`cancelled`、`refunded`。
3. 金额必须用 `decimal.Decimal`，禁止经过 `float`。
4. 无效行写入 `rejected_orders.csv`，列严格为 `source_line,order_id,reason`。`source_line` 是原 CSV 的物理行号（表头为第 1 行）；`reason` 只需是非空、可读说明。
5. 合法行按清洗后的 `order_id` 去重，保留第一次出现；后续重复行不进 cleaned，也不进 rejected，只计入 `duplicate_orders`。
6. `cleaned_orders.csv` 列严格为：
   `order_id,order_date,region,product,quantity,unit_price,status,line_total`
   其中价格和行金额固定两位小数；按 `order_date`、`order_id` 升序。
7. 营收只统计 `paid` 行；取消和退款行仍保留在 cleaned 中，但不计入营收与 paid 数量。

## `summary.json` 形状

必须包含且只包含：

```json
{
  "valid_orders": 0,
  "duplicate_orders": 0,
  "rejected_orders": 0,
  "paid_orders": 0,
  "paid_units": 0,
  "revenue": "0.00",
  "revenue_by_region": {"地区": "0.00"},
  "revenue_by_product": {"产品": "0.00"}
}
```

所有金额固定两位小数。用 `ensure_ascii=False` 写出可读中文。

## `report.md`

标题必须为 `# 销售清洗报告`，并清楚写出有效、重复、拒绝和 paid 订单数、paid 件数、总营收，以及营收最高地区。报告中的数字必须来自本次计算，不能手填另一套结果。

完成前自行重新读取输出并核对列名、行数、金额和 JSON。
