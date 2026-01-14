# 预测市场单文件实现功能文档

> **版本**: Phase 2.1
> **日期**: 2025-01-14
> **文件**: `src/core/orderbook/prediction.rs`
> **测试**: `tests/prediction_single_file_test.rs`

---

## 概述

本文档描述了 matching-core CLOB 引擎的**单文件预测市场实现**，该实现提供了完整的预测市场订单撮合功能，包括三种撮合模式 (NORMAL/MINT/MERGE) 和跨簿快照/状态恢复功能。

### 设计目标

1. **单文件架构**: 所有预测市场功能集中在一个文件中，便于理解和维护
2. **模块化组织**: 通过清晰的注释分隔不同功能模块
3. **完整功能覆盖**: 支持所有预测市场核心功能
4. **高性能**: 复用 `DirectOrderBookOptimized` 的高性能撮合引擎

---

## 架构设计

### 文件结构

```
src/core/orderbook/prediction.rs (1339 行)
├── TYPES (类型定义)
│   ├── MarketId, TokenId
│   ├── TokenType (YES/NO)
│   ├── MatchType (Normal/Mint/Merge)
│   └── PRICE_SCALE 常量
│
├── SERVICE FLAGS & MATCH TYPE ENCODING (Phase 2.1)
│   ├── service_flags::CROSS_BOOK_MATCH
│   └── match_type_encoding (编解码撮合类型)
│
├── SNAPSHOT TYPES (快照类型)
│   ├── UnifiedOrderBookSnapshot
│   └── MarketConfig
│
├── ORDER (订单)
│   └── PredictionOrder
│
├── CONVERTER (转换器)
│   └── OrderConverter
│
├── EVENTS (事件)
│   └── PredictionTradeEvent
│
├── UNIFIED ORDER BOOK (统一订单簿)
│   └── UnifiedOrderBook
│
└── TESTS (单元测试)
    └── 11 个测试函数
```

---

## 核心类型

### 1. TokenType - 代币类型

```rust
pub enum TokenType {
    YES,  // 事件会发生
    NO,   // 事件不会发生
}

impl TokenType {
    pub fn complement(&self) -> Self {
        match self {
            TokenType::YES => TokenType::NO,
            TokenType::NO => TokenType::YES,
        }
    }
}
```

**关键特性**: 互补代币自动转换

### 2. MatchType - 撮合类型

```rust
pub enum MatchType {
    Normal,  // 标准买卖撮合
    Mint,    // 两个互补代币买单铸造新代币对
    Merge,   // 两个互补代币卖单合并为抵押品
}
```

**撮合条件**:
- **Normal**: 买单 vs 卖单 (价格匹配)
- **Mint**: price_yes + price_no ≥ PRICE_SCALE
- **Merge**: price_yes + price_no ≤ PRICE_SCALE

### 3. PRICE_SCALE - 价格精度

```rust
pub const PRICE_SCALE: i64 = 1_000_000;
```

**用途**: USDC 6 decimals 价格精度

**示例**:
- 0.65 USDC → 650,000
- 0.35 USDC → 350,000
- 互补性: 650_000 + 350_000 = 1_000_000

---

## 订单系统

### PredictionOrder - 预测市场订单

```rust
pub struct PredictionOrder {
    pub market_id: MarketId,      // 市场 ID
    pub token_type: TokenType,    // YES 或 NO
    pub maker: UserId,            // 下单用户
    pub making_amount: Size,      // 卖出的 ERC1155 代币数量
    pub taking_amount: Size,      // 收到的 USDC 数量 (6 decimals)
    pub price: Price,             // 预计算的价格
    pub side: OrderAction,        // Bid (买) 或 Ask (卖)
    pub expiration: i64,          // 过期时间戳
    pub salt: u64,                // 随机数
}
```

**价格计算**:
```rust
price = taking_amount / making_amount
```

**示例**:
```rust
// 创建 YES 买单：以 0.65 USDC 价格买入 100 个 YES 代币
let order = PredictionOrder::new(
    1,                      // market_id
    TokenType::YES,
    100,                    // user_id
    100,                    // making_amount (tokens)
    65_000_000,             // taking_amount (65 USDC, 6 decimals)
    OrderAction::Bid,
    i64::MAX,               // 永不过期
);
assert_eq!(order.price, 650_000); // 65_000_000 / 100
```

---

## 订单转换器

### OrderConverter

负责将预测市场订单转换为 `OrderCommand`。

#### Symbol ID 编码方案

```text
symbol_id = (market_id << 1) | token_bit

token_bit:
  - 0 for YES
  - 1 for NO
```

**示例**:
| Market ID | Token Type | Symbol ID |
|-----------|------------|-----------|
| 1 | YES | 2 (0b10) |
| 1 | NO | 3 (0b11) |
| 100 | YES | 200 (0b11001000) |
| 100 | NO | 201 (0b11001001) |

#### API 转换

```rust
pub fn to_order_command(
    order: &PredictionOrder,
    order_id: OrderId,
    timestamp: i64,
) -> OrderCommand
```

**转换说明**:
- `market_id` + `token_type` → `symbol_id` (编码)
- `price` 保持不变 (已预计算)
- `size` = `making_amount` (代币数量)
- `action` 直接映射
- `order_type` = `Gtc` (Phase 1)

---

## 统一订单簿

### UnifiedOrderBook

管理 YES 和 NO 代币的订单，实现互补代币撮合。

#### 核心结构

```rust
pub struct UnifiedOrderBook {
    pub(crate) market_id: MarketId,
    pub(crate) yes_book: DirectOrderBookOptimized,  // YES 订单簿
    pub(crate) no_book: DirectOrderBookOptimized,   // NO 订单簿
    pub(crate) price_scale: i64,
}
```

#### 四种撮合策略

| 订单类型 | 方向 | 优先撮合 (NORMAL) | 次要撮合 (MINT/MERGE) |
|---------|------|------------------|---------------------|
| YES | 买 | YES 买 ↔ YES 卖 | YES 买 ↔ NO 买 (MINT) |
| YES | 卖 | YES 卖 ↔ YES 买 | YES 卖 ↔ NO 卖 (MERGE) |
| NO | 买 | NO 买 ↔ NO 卖 | NO 买 ↔ YES 买 (MINT) |
| NO | 卖 | NO 卖 ↔ NO 买 | NO 卖 ↔ YES 卖 (MERGE) |

#### 镜像订单机制

MINT/MERGE 撮合通过在对手方订单簿中创建"镜像订单"实现：

```rust
// 示例：YES 买单的 MINT 撮合
let complement_price = PRICE_SCALE - cmd.price;  // 计算互补价格
let mut counter_cmd = OrderCommand {
    price: complement_price,        // 使用互补价格
    action: OrderAction::Ask,       // 方向反转：Bid → Ask
    order_type: OrderType::Ioc,     // IOC 立即成交或取消
    size: remaining,                // NORMAL 撮合后的剩余量
    events_group: match_type_encoding::encode(MatchType::Mint, timestamp),
    service_flags: service_flags::CROSS_BOOK_MATCH,
    ...
};

// 在对手方订单簿 (no_book) 中撮合
self.no_book.new_order(&mut counter_cmd);

// 合并撮合事件到原始订单
for event in counter_cmd.matcher_events {
    cmd.matcher_events.push(...);
}
```

**价格转换规则**:

| 原始订单 | 镜像订单方向 | 镜像订单价格 | 撮合订单簿 |
|---------|------------|------------|-----------|
| YES Bid | Ask | PRICE_SCALE - price_yes | no_book |
| YES Ask | Bid | PRICE_SCALE - price_yes | no_book |
| NO Bid | Ask | PRICE_SCALE - price_no | yes_book |
| NO Ask | Bid | PRICE_SCALE - price_no | yes_book |

**关键特性**:
- IOC 订单确保不会持久化
- 每个订单簿只维护自己类型的订单
- 撮合事件是跨簿交互的唯一记录

---

## Phase 2.1 新增功能

### 1. Service Flags

```rust
pub mod service_flags {
    /// 跨簿撮合标记（用于 MINT/MERGE）
    pub const CROSS_BOOK_MATCH: i32 = 0x01;
}
```

### 2. Match Type Encoding

将撮合类型编码到 `events_group` 高位：

```rust
pub mod match_type_encoding {
    /// 高位掩码（高 8 位用于撮合类型）
    pub const MASK: u64 = 0xFF00000000000000;
    pub const NORMAL: u64 = 0x0100000000000000;
    pub const MINT: u64 = 0x0200000000000000;
    pub const MERGE: u64 = 0x0300000000000000;

    /// 编码撮合类型到 events_group
    pub fn encode(match_type: MatchType, counter: u64) -> u64;

    /// 从 events_group 解码撮合类型
    pub fn decode(events_group: u64) -> Option<MatchType>;
}
```

### 3. 跨簿快照

```rust
pub struct UnifiedOrderBookSnapshot {
    pub yes_book_snapshot: DirectOrderBookOptimized,
    pub no_book_snapshot: DirectOrderBookOptimized,
    pub timestamp: i64,
    pub checksum: u64,
}
```

**API**:
```rust
pub fn take_snapshot(&self) -> UnifiedOrderBookSnapshot;
pub fn restore_snapshot(&mut self, snapshot: UnifiedOrderBookSnapshot) -> anyhow::Result<()>;
pub fn verify_cross_book_consistency(&self) -> anyhow::Result<bool>;
```

**校验和计算**:
使用订单簿的关键状态信息（订单数量、深度等）计算校验和。

---

## 撮合事件

### PredictionTradeEvent

扩展基础撮合事件，添加预测市场特定信息：

```rust
pub struct PredictionTradeEvent {
    pub base_event: MatcherTradeEvent,  // 基础事件
    pub match_type: MatchType,           // 撮合类型
    pub token_type: TokenType,           // 订单代币
    pub counter_token_type: TokenType,   // 对手方代币
}
```

**获取对手方信息**:

| 信息 | 获取方式 |
|-----|---------|
| 对手方订单 ID | `base_event.matched_order_id` |
| 对手方用户 ID | `base_event.matched_order_uid` |
| 对手方代币类型 | `counter_token_type` |

---

## 测试覆盖

### 单元测试 (11 个)

**TokenType 测试**:
- `test_token_type_complement`

**PredictionOrder 测试**:
- `test_order_creation`
- `test_order_with_salt`
- `test_complement_price`

**OrderConverter 测试**:
- `test_symbol_id_encoding_yes`
- `test_symbol_id_encoding_no`
- `test_symbol_id_decoding`
- `test_to_order_command`

**UnifiedOrderBook 测试**:
- `test_unified_orderbook_creation`
- `test_unified_orderbook_complement_price`
- `test_are_complementary`
- `test_get_l2_data`

### 端到端测试 (15 个)

**NORMAL 撮合测试** (3 个):
- `test_normal_yes_bid_ask_full_fill` - YES 完全成交
- `test_normal_yes_bid_ask_partial_fill` - YES 部分成交
- `test_normal_no_bid_ask_full_fill` - NO 完全成交

**MINT 撮合测试** (3 个):
- `test_mint_yes_no_bids_exact_match` - 精确匹配
- `test_mint_yes_no_bids_surplus` - 有剩余量
- `test_mint_no_yes_bids` - 反向顺序

**MERGE 撮合测试** (3 个):
- `test_merge_yes_no_asks_exact_match` - 精确匹配
- `test_merge_yes_no_asks_surplus` - 有剩余量
- `test_merge_no_yes_asks` - 反向顺序

**复杂场景测试** (3 个):
- `test_mint_then_merge_sequence` - MINT → MERGE 序列
- `test_multi_user_matching` - 多用户撮合
- `test_price_boundaries` - 边界价格 (0.99 + 0.01)

**Phase 2.1 测试** (3 个):
- `test_snapshot_and_restore` - 快照与恢复
- `test_cross_book_snapshot_with_mint` - MINT 后的跨簿快照
- `test_service_flags_cross_book_marking` - 跨簿标记
- `test_cross_book_consistency_validation` - 一致性验证

---

## API 使用示例

### 创建订单簿

```rust
use matching_core::core::orderbook::prediction::*;

let market = UnifiedOrderBook::new(1);
```

### 创建订单

```rust
let order = PredictionOrder::new(
    1,                      // market_id
    TokenType::YES,         // token_type
    100,                    // user_id
    100,                    // making_amount (tokens)
    65_000_000,             // taking_amount (65 USDC, 6 decimals)
    OrderAction::Bid,       // side
    i64::MAX,               // expiration
);
// price = 65_000_000 / 100 = 650_000 (0.65)
```

### 下单与撮合

```rust
let mut cmd = OrderConverter::to_order_command(&order, order_id, timestamp);
market.place_order(&mut cmd);

// 检查撮合结果
for event in &cmd.matcher_events {
    println!("Trade: size={}, price={}", event.size, event.price);
}

// 检查剩余量
if cmd.size == 0 {
    println!("订单完全成交");
}
```

### 查询订单簿状态

```rust
let l2_data = market.get_l2_data(TokenType::YES, 10);

for (i, price) in l2_data.ask_prices.iter().enumerate() {
    let volume = l2_data.ask_volumes[i];
    println!("Ask: @{} volume={}", price, volume);
}
```

### 快照与恢复

```rust
// 创建快照
let snapshot = market.take_snapshot();
println!("Snapshot: timestamp={}, checksum={}",
         snapshot.timestamp, snapshot.checksum);

// 验证状态一致性
let consistent = market.verify_cross_book_consistency().unwrap();
assert!(consistent);

// 恢复快照
let mut restored_market = UnifiedOrderBook::new(1);
restored_market.restore_snapshot(snapshot).unwrap();
```

---

## 关键技术点

### 1. 价格精度

```rust
PRICE_SCALE = 1_000_000

内部表示：0.65 → 650_000
显示格式：650_000 / 1_000_000 = 0.65
```

### 2. 价格互补性

```rust
complement_price = PRICE_SCALE - price

例如：
YES @ 0.65 → NO @ 0.35
complement_price(650_000) = 350_000
```

### 3. 撮合优先级

```rust
1. NORMAL - 相同代币类型买卖撮合
2. MINT/MERGE - 互补代币撮合
```

### 4. 订单状态更新

```rust
// 撮合完成后
cmd.size = cmd.size.saturating_sub(filled);

// 完全成交：cmd.size = 0
// 部分成交：cmd.size = 剩余量
// 未成交：  cmd.size = 原始量
```

### 5. 对手方订单簿访问

**镜像订单机制**:

```rust
// 访问方式：创建镜像订单
let complement_price = self.complement_price(cmd.price);
let mut counter_cmd = OrderCommand {
    price: complement_price,    // 互补价格
    action: action.flip(),      // 方向反转
    order_type: OrderType::Ioc, // IOC 立即执行
    ...
};

// 在对手方簿中撮合
let book = match token_type {
    TokenType::YES => &mut self.no_book,
    TokenType::NO => &mut self.yes_book,
};
book.new_order(&mut counter_cmd);
```

---

## Phase 1 当前限制

- [ ] 对手方订单簿状态不自动更新
- [ ] 不提供对手方订单簿的直接引用
- [ ] 不支持跨簿订单簿状态快照 (Phase 2.1 已实现)
- [ ] 事件不包含对手方订单簿类型字段
- [ ] 簿本状态一致性验证 (Phase 2.1 已实现)

---

## Phase 2 预留功能

- [ ] EIP712 签名验证
- [ ] 以太坊合约交互
- [ ] Relayer 层集成
- [ ] 链上结算支持

---

## 与 src/prediction/ 的区别

| 特性 | src/prediction/ (多文件) | src/core/orderbook/prediction.rs (单文件) |
|-----|-------------------------|----------------------------------------|
| 文件组织 | 7 个独立文件 | 1 个文件 (1339 行) |
| 功能完整性 | ✅ 完整 | ✅ 完整 |
| 测试覆盖 | prediction_e2e_test.rs | prediction_single_file_test.rs |
| Phase 2.1 | ❌ 未实现 | ✅ 已实现快照/一致性验证 |
| 适用场景 | 大型项目、团队协作 | 小型项目、快速原型 |

---

## 文件清单

### 单文件实现
- `src/core/orderbook/prediction.rs` - 主实现文件
- `tests/prediction_single_file_test.rs` - 端到端测试

### 修改的文件
- `src/lib.rs` - 添加 `pub mod prediction;`
- `src/core/orderbook.rs` - 声明 prediction 子模块
- `src/core/orderbook/direct_optimized.rs` - 添加 Debug trait

---

## 参考资源

- `POLYMARKET_FEASIBILITY_ASSESSMENT.md` - 可行性评估
- `PREDICTION_MODULE_FEATURES.md` - 多文件模块功能文档
- Gnosis Conditional Tokens Framework
- CTF Exchange 合约规范

---

## 版本信息

- **版本**: v0.2.2 (Phase 2.1)
- **日期**: 2025-01-14
- **Phase**: Phase 1 (本地匹配) + Phase 2.1 (快照与一致性验证)
- **测试覆盖**: 26 个测试全部通过
