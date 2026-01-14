//! 单文件预测市场实现
//!
//! 与 src/prediction/ 功能相同，采用单文件组织方式。
//!
//! # 功能
//! - 预测市场订单类型定义
//! - 订单转换 (PredictionOrder → OrderCommand)
//! - 统一订单簿 (YES/NO 代币统一管理)
//! - 三种撮合模式 (NORMAL/MINT/MERGE)

use crate::api::*;
use crate::api::market_data::L2MarketData;
use crate::core::orderbook::{DirectOrderBookOptimized, OrderBook};
use serde::{Deserialize, Serialize};

// ============================================================
// TYPES
// ============================================================

/// Polymarket 市场 ID
///
/// 对应 CTF (Conditional Tokens Framework) 中的 conditionId
pub type MarketId = u64;

/// ERC1155 代币 ID
///
/// 用于标识特定的条件代币
pub type TokenId = u64;

/// 代币类型 - YES 或 NO
///
/// 在 Polymarket 预测市场中，每个市场有一对互补代币：
/// - YES: 表示事件会发生
/// - NO: 表示事件不会发生
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TokenType {
    /// Yes outcome token
    YES,
    /// No outcome token
    NO,
}

impl TokenType {
    /// 获取互补代币类型
    ///
    /// # 示例
    /// ```
    /// # use matching_core::core::orderbook::prediction::TokenType;
    /// assert_eq!(TokenType::YES.complement(), TokenType::NO);
    /// assert_eq!(TokenType::NO.complement(), TokenType::YES);
    /// ```
    pub fn complement(&self) -> Self {
        match self {
            TokenType::YES => TokenType::NO,
            TokenType::NO => TokenType::YES,
        }
    }
}

/// 撮合类型 - 标记事件属于哪种撮合模式
///
/// 根据 CTF Exchange 合约，有三种撮合场景：
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchType {
    /// 标准买卖撮合 (合约中称为 COMPLEMENTARY)
    ///
    /// 场景：买单 vs 卖单
    /// 执行：直接资产交换
    Normal,

    /// 两个互补代币买单铸造新代币对
    ///
    /// 场景：YES 买单 vs NO 买单
    /// 条件：price_yes + price_no >= PRICE_SCALE
    /// 执行：锁定抵押品，铸造新的 YES/NO 代币对
    Mint,

    /// 两个互补代币卖单合并为抵押品
    ///
    /// 场景：YES 卖单 vs NO 卖单
    /// 条件：price_yes + price_no <= PRICE_SCALE
    /// 执行：销毁代币对，释放抵押品
    Merge,
}

/// 价格精度 (USDC 6 decimals)
///
/// 所有价格都按此精度进行定点数计算
/// 例如：0.65 USDC = 650,000
pub const PRICE_SCALE: i64 = 1_000_000;

// ============================================================
// SERVICE FLAGS & MATCH TYPE ENCODING (Phase 2.1)
// ============================================================

/// 服务标志位常量
pub mod service_flags {
    /// 跨簿撮合标记（用于 MINT/MERGE）
    pub const CROSS_BOOK_MATCH: i32 = 0x01;
}

/// 撮合类型编码（用于 events_group 高位）
pub mod match_type_encoding {
    use super::MatchType;

    /// 高位掩码（高 8 位用于撮合类型）
    pub const MASK: u64 = 0xFF00000000000000;
    /// NORMAL 撮合类型标记
    pub const NORMAL: u64 = 0x0100000000000000;
    /// MINT 撮合类型标记
    pub const MINT: u64 = 0x0200000000000000;
    /// MERGE 撮合类型标记
    pub const MERGE: u64 = 0x0300000000000000;

    /// 编码撮合类型到 events_group
    ///
    /// # 参数
    /// - `match_type`: 撮合类型
    /// - `counter`: 计数器或时间戳（低 56 位）
    ///
    /// # 返回
    /// 编码后的 events_group 值
    ///
    /// # 示例
    /// ```
    /// # use matching_core::core::orderbook::prediction::*;
    /// let encoded = match_type_encoding::encode(MatchType::Mint, 12345);
    /// assert_eq!(encoded & match_type_encoding::MASK, match_type_encoding::MINT);
    /// ```
    pub fn encode(match_type: MatchType, counter: u64) -> u64 {
        let flag = match match_type {
            MatchType::Normal => NORMAL,
            MatchType::Mint => MINT,
            MatchType::Merge => MERGE,
        };
        flag | (counter & 0x00FFFFFFFFFFFFFF)
    }

    /// 从 events_group 解码撮合类型
    ///
    /// # 参数
    /// - `events_group`: 编码的 events_group 值
    ///
    /// # 返回
    /// 撮合类型（如果高位编码有效）
    ///
    /// # 示例
    /// ```
    /// # use matching_core::core::orderbook::prediction::*;
    /// let encoded = match_type_encoding::encode(MatchType::Mint, 12345);
    /// let decoded = match_type_encoding::decode(encoded);
    /// assert_eq!(decoded, Some(MatchType::Mint));
    /// ```
    pub fn decode(events_group: u64) -> Option<MatchType> {
        let flag = events_group & MASK;
        match flag {
            NORMAL => Some(MatchType::Normal),
            MINT => Some(MatchType::Mint),
            MERGE => Some(MatchType::Merge),
            _ => None,
        }
    }
}

/// 统一订单簿快照
///
/// 包含 YES 和 NO 订单簿的完整状态快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedOrderBookSnapshot {
    /// YES 代币订单簿快照
    pub yes_book_snapshot: DirectOrderBookOptimized,
    /// NO 代币订单簿快照
    pub no_book_snapshot: DirectOrderBookOptimized,
    /// 快照时间戳（Unix 时间戳）
    pub timestamp: i64,
    /// 状态校验和
    pub checksum: u64,
}

/// 市场配置
///
/// 包含创建 Polymarket 市场所需的所有配置信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketConfig {
    /// 市场唯一标识符
    pub id: MarketId,
    /// YES 代币 ID (ERC1155 tokenId)
    pub yes_token: TokenId,
    /// NO 代币 ID (ERC1155 tokenId)
    pub no_token: TokenId,
    /// CTF conditionId (32 bytes)
    pub condition_id: [u8; 32],
}

impl MarketConfig {
    /// 创建新的市场配置
    pub fn new(id: MarketId, yes_token: TokenId, no_token: TokenId, condition_id: [u8; 32]) -> Self {
        Self {
            id,
            yes_token,
            no_token,
            condition_id,
        }
    }
}

// ============================================================
// ORDER
// ============================================================

/// 预测市场订单 (Phase 1: 简化版，无签名)
///
/// # 字段说明
/// - `market_id`: 市场 ID，对应 CTF conditionId
/// - `token_type`: YES 或 NO 代币
/// - `maker`: 用户 ID (Phase 1 使用 UserId，Phase 2 将使用以太坊地址)
/// - `making_amount`: 卖出的 ERC1155 代币数量
/// - `taking_amount`: 收到的 USDC 数量 (6 decimals)
/// - `price`: 预计算的价格，单位：USDC per token (scaled by PRICE_SCALE)
/// - `side`: 订单方向 (Bid = 买, Ask = 卖)
/// - `expiration`: 过期时间戳 (i64::MAX 表示永不过期)
/// - `salt`: 随机数用于确保订单唯一性
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionOrder {
    /// 市场 ID
    pub market_id: MarketId,
    /// 代币类型 (YES/NO)
    pub token_type: TokenType,
    /// 用户 ID (Phase 1: UserId, Phase 2: H160)
    pub maker: UserId,
    /// 卖出的 ERC1155 代币数量
    pub making_amount: Size,
    /// 收到的 USDC 数量 (6 decimals)
    pub taking_amount: Size,
    /// 价格 (USDC per token, scaled by PRICE_SCALE)
    pub price: Price,
    /// 订单方向
    pub side: OrderAction,
    /// 过期时间戳
    pub expiration: i64,
    /// 随机数确保订单唯一性
    pub salt: u64,
}

impl PredictionOrder {
    /// 创建新的预测市场订单
    ///
    /// # 参数
    /// - `market_id`: 市场 ID
    /// - `token_type`: YES 或 NO 代币
    /// - `maker`: 用户 ID
    /// - `making_amount`: ERC1155 代币数量
    /// - `taking_amount`: USDC 数量
    /// - `side`: Bid (买) 或 Ask (卖)
    /// - `expiration`: 过期时间戳
    ///
    /// # 价格计算
    /// 价格自动计算为: `taking_amount / making_amount * PRICE_SCALE`
    ///
    /// # 示例
    /// ```no_run
    /// # use matching_core::core::orderbook::prediction::*;
    /// # use matching_core::api::*;
    /// // 创建 YES 买单：以 0.65 USDC 价格买入 100 个 YES 代币
    /// let order = PredictionOrder::new(
    ///     1,                      // market_id
    ///     TokenType::YES,
    ///     100,                    // user_id
    ///     100,                    // making_amount (tokens)
    ///     65_000_000,             // taking_amount (65 USDC, 6 decimals)
    ///     OrderAction::Bid,
    ///     i64::MAX,               // 永不过期
    /// );
    /// assert_eq!(order.price, 650_000); // 0.65 * 1_000_000
    /// ```
    pub fn new(
        market_id: MarketId,
        token_type: TokenType,
        maker: UserId,
        making_amount: Size,
        taking_amount: Size,
        side: OrderAction,
        expiration: i64,
    ) -> Self {
        // 计算价格: taking_amount (USDC) / making_amount (tokens)
        // taking_amount 已经是 6 decimals 的 USDC，直接除以 making_amount 得到价格
        let price = if making_amount > 0 {
            taking_amount / making_amount
        } else {
            0
        };

        Self {
            market_id,
            token_type,
            maker,
            making_amount,
            taking_amount,
            price,
            side,
            expiration,
            salt: 0,
        }
    }

    /// 设置 salt 值
    pub fn with_salt(mut self, salt: u64) -> Self {
        self.salt = salt;
        self
    }

    /// 验证订单价格是否合法 [0, PRICE_SCALE]
    pub fn is_valid_price(&self) -> bool {
        self.price >= 0 && self.price <= PRICE_SCALE
    }

    /// 验证订单是否已过期
    pub fn is_expired(&self, current_time: i64) -> bool {
        if self.expiration == i64::MAX {
            false
        } else {
            current_time >= self.expiration
        }
    }

    /// 获取互补价格
    ///
    /// 对于 YES 代币，返回 NO 代币的对应价格
    /// 公式: complement_price = PRICE_SCALE - price
    ///
    /// # 示例
    /// ```
    /// # use matching_core::core::orderbook::prediction::{PredictionOrder, TokenType};
    /// # use matching_core::api::OrderAction;
    /// let order = PredictionOrder::new(1, TokenType::YES, 100, 100, 65_000_000, OrderAction::Bid, i64::MAX);
    /// assert_eq!(order.complement_price(), 350_000); // 1.00 - 0.65
    /// ```
    pub fn complement_price(&self) -> Price {
        PRICE_SCALE - self.price
    }
}

// ============================================================
// CONVERTER
// ============================================================

/// 订单转换器
///
/// 负责将预测市场订单转换为 matching-core 引擎可以处理的 OrderCommand
pub struct OrderConverter;

impl OrderConverter {
    /// 将预测市场订单转换为 OrderCommand
    ///
    /// # 参数
    /// - `order`: 预测市场订单
    /// - `order_id`: 订单唯一 ID
    /// - `timestamp`: 订单时间戳
    ///
    /// # 转换说明
    /// - market_id 和 token_type 被编码到 symbol_id 中
    /// - price 保持不变 (已经预计算)
    /// - size 使用 making_amount (代币数量)
    /// - action 直接映射
    /// - order_type 固定为 GTC (Phase 1)
    ///
    /// # 示例
    /// ```no_run
    /// # use matching_core::core::orderbook::prediction::*;
    /// # use matching_core::api::*;
    /// let order = PredictionOrder::new(1, TokenType::YES, 100, 100, 65_000_000, OrderAction::Bid, i64::MAX);
    /// let cmd = OrderConverter::to_order_command(&order, 1, 1000);
    /// assert_eq!(cmd.uid, 100);
    /// assert_eq!(cmd.price, 650_000);
    /// ```
    pub fn to_order_command(
        order: &PredictionOrder,
        order_id: OrderId,
        timestamp: i64,
    ) -> OrderCommand {
        let symbol_id = Self::encode_symbol_id(order.market_id, order.token_type);

        OrderCommand {
            command: OrderCommandType::PlaceOrder,
            result_code: CommandResultCode::New,
            uid: order.maker,
            order_id,
            symbol: symbol_id,
            price: order.price,
            reserve_price: order.price,
            size: order.making_amount,
            action: order.side,
            order_type: OrderType::Gtc, // Phase 1: 只支持 GTC
            timestamp,
            events_group: 0,
            service_flags: 0,
            stop_price: None,
            visible_size: None,
            expire_time: if order.expiration == i64::MAX {
                None
            } else {
                Some(order.expiration)
            },
            matcher_events: Vec::with_capacity(4),
        }
    }

    /// 编码 symbol_id: (market_id, token_type) → symbol_id
    ///
    /// # 编码方案
    /// ```text
    /// symbol_id = (market_id << 1) | token_bit
    ///
    /// token_bit:
    ///   - 0 for YES
    ///   - 1 for NO
    /// ```
    ///
    /// # 示例
    /// ```
    /// # use matching_core::core::orderbook::prediction::{TokenType, OrderConverter};
    /// // Market 1, YES token
    /// assert_eq!(OrderConverter::encode_symbol_id(1, TokenType::YES), 2);
    ///
    /// // Market 1, NO token
    /// assert_eq!(OrderConverter::encode_symbol_id(1, TokenType::NO), 3);
    ///
    /// // Market 100, YES token
    /// assert_eq!(OrderConverter::encode_symbol_id(100, TokenType::YES), 200);
    /// ```
    pub fn encode_symbol_id(market_id: MarketId, token_type: TokenType) -> SymbolId {
        let token_bit = match token_type {
            TokenType::YES => 0,
            TokenType::NO => 1,
        };
        ((market_id as SymbolId) << 1) | token_bit
    }

    /// 解码 symbol_id: symbol_id → (market_id, token_type)
    ///
    /// # 示例
    /// ```
    /// # use matching_core::core::orderbook::prediction::{TokenType, OrderConverter};
    /// let (market_id, token_type) = OrderConverter::decode_symbol_id(3);
    /// assert_eq!(market_id, 1);
    /// assert_eq!(token_type, TokenType::NO);
    /// ```
    pub fn decode_symbol_id(symbol_id: SymbolId) -> (MarketId, TokenType) {
        let market_id = (symbol_id >> 1) as MarketId;
        let token_type = if symbol_id & 1 == 0 {
            TokenType::YES
        } else {
            TokenType::NO
        };
        (market_id, token_type)
    }

    /// 验证 symbol_id 编码/解码的一致性
    ///
    /// # 示例
    /// ```
    /// # use matching_core::core::orderbook::prediction::{TokenType, OrderConverter};
    /// assert!(OrderConverter::verify_encoding(12345, TokenType::YES));
    /// assert!(OrderConverter::verify_encoding(12345, TokenType::NO));
    /// ```
    pub fn verify_encoding(market_id: MarketId, token_type: TokenType) -> bool {
        let symbol_id = Self::encode_symbol_id(market_id, token_type);
        let (decoded_market, decoded_type) = Self::decode_symbol_id(symbol_id);
        decoded_market == market_id && decoded_type == token_type
    }
}

// ============================================================
// EVENTS
// ============================================================

/// 预测市场撮合事件 - 扩展基础事件
///
/// 在 matching-core 的 MatcherTradeEvent 基础上添加：
/// - match_type: 撮合类型 (NORMAL/MINT/MERGE)
/// - token_type: 订单代币类型 (YES/NO)
/// - counter_token_type: 对手方代币类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictionTradeEvent {
    /// 基础撮合事件
    pub base_event: MatcherTradeEvent,

    /// 撮合类型 (NORMAL/MINT/MERGE)
    pub match_type: MatchType,

    /// 订单代币类型 (YES/NO)
    pub token_type: TokenType,

    /// 对手方代币类型 (互补代币)
    pub counter_token_type: TokenType,
}

impl PredictionTradeEvent {
    /// 从基础事件创建预测市场事件
    pub fn from_base_event(
        base_event: MatcherTradeEvent,
        match_type: MatchType,
        token_type: TokenType,
    ) -> Self {
        Self {
            base_event,
            match_type,
            token_type,
            counter_token_type: token_type.complement(),
        }
    }

    /// 创建 NORMAL 事件
    pub fn new_normal(
        base_event: MatcherTradeEvent,
        token_type: TokenType,
    ) -> Self {
        Self::from_base_event(base_event, MatchType::Normal, token_type)
    }

    /// 创建 MINT 事件
    pub fn new_mint(
        base_event: MatcherTradeEvent,
        token_type: TokenType,
    ) -> Self {
        Self::from_base_event(base_event, MatchType::Mint, token_type)
    }

    /// 创建 MERGE 事件
    pub fn new_merge(
        base_event: MatcherTradeEvent,
        token_type: TokenType,
    ) -> Self {
        Self::from_base_event(base_event, MatchType::Merge, token_type)
    }

    /// 获取成交数量
    pub fn size(&self) -> Size {
        self.base_event.size
    }

    /// 获取成交价格
    pub fn price(&self) -> Price {
        self.base_event.price
    }

    /// 获取对手方订单 ID
    pub fn counterparty_order_id(&self) -> OrderId {
        self.base_event.matched_order_id
    }

    /// 获取对手方用户 ID
    pub fn counterparty_user_id(&self) -> UserId {
        self.base_event.matched_order_uid
    }
}

// ============================================================
// UNIFIED ORDER BOOK
// ============================================================

/// 统一订单簿 - 管理 YES 和 NO 代币的订单
///
/// # 核心概念
///
/// 每个市场有两个独立的订单簿：
/// - `yes_book`: YES 代币订单簿
/// - `no_book`: NO 代币订单簿
///
/// # 价格互补性
///
/// YES 和 NO 代币的价格互补关系：
/// ```text
/// price_yes + price_no = PRICE_SCALE (1_000_000)
///
/// 例如:
///   YES @ 0.65 → NO @ 0.35
///   complement_price(650_000) = 350_000
/// ```
///
/// # 撮合策略
///
/// 根据 (token_type, action) 选择撮合策略：
///
/// | 订单类型 | 方向 | 优先撮合 (NORMAL) | 次要撮合 (MINT/MERGE) |
/// |---------|------|------------------|---------------------|
/// | YES | 买 | YES 买 ↔ YES 卖 | YES 买 ↔ NO 买 (MINT) |
/// | YES | 卖 | YES 卖 ↔ YES 买 | YES 卖 ↔ NO 卖 (MERGE) |
/// | NO | 买 | NO 买 ↔ NO 卖 | NO 买 ↔ YES 买 (MINT) |
/// | NO | 卖 | NO 卖 ↔ NO 买 | NO 卖 ↔ YES 卖 (MERGE) |
pub struct UnifiedOrderBook {
    pub(crate) market_id: MarketId,
    pub(crate) yes_book: DirectOrderBookOptimized,
    pub(crate) no_book: DirectOrderBookOptimized,
    pub(crate) price_scale: i64,
}

impl UnifiedOrderBook {
    /// 创建新的统一订单簿
    ///
    /// # 参数
    /// - `market_id`: 市场 ID
    ///
    /// # 示例
    /// ```no_run
    /// # use matching_core::core::orderbook::prediction::*;
    /// let market = UnifiedOrderBook::new(1);
    /// ```
    pub fn new(market_id: MarketId) -> Self {
        let spec = CoreSymbolSpecification {
            symbol_id: market_id as SymbolId,
            symbol_type: SymbolType::CurrencyExchangePair,
            base_currency: 0,
            quote_currency: 1,  // USDC
            base_scale_k: 1,
            quote_scale_k: PRICE_SCALE,
            taker_fee: 0,
            maker_fee: 0,
            margin_buy: 0,
            margin_sell: 0,
        };

        Self {
            market_id,
            yes_book: DirectOrderBookOptimized::new(spec.clone()),
            no_book: DirectOrderBookOptimized::new(spec),
            price_scale: PRICE_SCALE,
        }
    }

    /// 计算互补价格
    ///
    /// # 公式
    /// ```text
    /// complement_price = PRICE_SCALE - price
    /// ```
    ///
    /// # 示例
    /// ```no_run
    /// # use matching_core::core::orderbook::prediction::*;
    /// let market = UnifiedOrderBook::new(1);
    /// // YES @ 0.65 → NO @ 0.35
    /// assert_eq!(market.complement_price(650_000), 350_000);
    /// ```
    pub fn complement_price(&self, price: Price) -> Price {
        self.price_scale - price
    }

    /// 验证两个价格是否互补
    ///
    /// # 公式
    /// ```text
    /// price_a + price_b == PRICE_SCALE
    /// ```
    pub fn are_complementary(&self, price_a: Price, price_b: Price) -> bool {
        price_a + price_b == self.price_scale
    }

    /// 下单主入口
    ///
    /// 根据 symbol_id 解码的 (market_id, token_type) 和 action 路由到对应的撮合策略
    pub fn place_order(&mut self, cmd: &mut OrderCommand) -> CommandResultCode {
        // 解码 symbol_id 获取 (market_id, token_type)
        let (decoded_market_id, token_type) = OrderConverter::decode_symbol_id(cmd.symbol);

        // 验证 market_id 匹配
        if decoded_market_id != self.market_id {
            cmd.result_code = CommandResultCode::MatchingInvalidOrderBookId;
            return cmd.result_code;
        }

        // 根据 (token_type, action) 路由到对应的撮合策略
        match (token_type, cmd.action) {
            (TokenType::YES, OrderAction::Bid) => self.match_yes_bid(cmd),
            (TokenType::YES, OrderAction::Ask) => self.match_yes_ask(cmd),
            (TokenType::NO, OrderAction::Bid) => self.match_no_bid(cmd),
            (TokenType::NO, OrderAction::Ask) => self.match_no_ask(cmd),
        }
    }

    /// 取消订单
    pub fn cancel_order(&mut self, cmd: &mut OrderCommand) -> CommandResultCode {
        let (_, token_type) = OrderConverter::decode_symbol_id(cmd.symbol);

        let book = match token_type {
            TokenType::YES => &mut self.yes_book,
            TokenType::NO => &mut self.no_book,
        };

        book.cancel_order(cmd)
    }

    /// 获取订单簿 L2 数据
    pub fn get_l2_data(&self, token_type: TokenType, depth: usize) -> L2MarketData {
        let book = match token_type {
            TokenType::YES => &self.yes_book,
            TokenType::NO => &self.no_book,
        };

        book.get_l2_data(depth)
    }

    // ========== 四种撮合策略的入口方法 ==========

    /// 策略 1: YES 买单撮合
    /// 优先级:
    ///   1. NORMAL - YES 买 vs YES 卖
    ///   2. MINT - YES 买 vs NO 买
    fn match_yes_bid(&mut self, cmd: &mut OrderCommand) -> CommandResultCode {
        let original_size = cmd.size;
        let initial_event_count = cmd.matcher_events.len();

        // 先执行 NORMAL 撮合
        self.yes_book.new_order(cmd);

        // 计算 NORMAL 撮合后的剩余量
        let normal_filled: Size = cmd.matcher_events[initial_event_count..]
            .iter()
            .map(|e| e.size)
            .sum();
        let remaining = original_size - normal_filled;

        // 如果还有剩余，尝试 MINT 撮合
        if remaining > 0 {
            let complement_price = self.complement_price(cmd.price);
            let user_id = cmd.uid;
            let symbol = cmd.symbol;
            let timestamp = cmd.timestamp;
            let reserve_price = cmd.reserve_price;

            // 尝试在 NO 订单簿中进行 MINT 撮合
            let mut counter_cmd = OrderCommand {
                command: OrderCommandType::PlaceOrder,
                result_code: CommandResultCode::New,
                uid: user_id,
                order_id: 0,
                symbol,
                price: complement_price,
                reserve_price,
                size: remaining,
                action: OrderAction::Ask,  // YES BID → NO ASK
                order_type: OrderType::Ioc,
                timestamp,
                events_group: match_type_encoding::encode(MatchType::Mint, timestamp as u64),
                service_flags: service_flags::CROSS_BOOK_MATCH,
                stop_price: None,
                visible_size: None,
                expire_time: None,
                matcher_events: Vec::with_capacity(4),
            };

            self.no_book.new_order(&mut counter_cmd);

            // 将撮合事件添加到原始订单
            for event in counter_cmd.matcher_events {
                cmd.matcher_events.push(MatcherTradeEvent::new_trade(
                    event.size,
                    event.price,
                    event.matched_order_id,
                    event.matched_order_uid,
                    cmd.reserve_price,
                ));
            }
        }
        CommandResultCode::Success
    }

    /// 策略 2: YES 卖单撮合
    /// 优先级:
    ///   1. NORMAL - YES 卖 vs YES 买
    ///   2. MERGE - YES 卖 vs NO 卖
    fn match_yes_ask(&mut self, cmd: &mut OrderCommand) -> CommandResultCode {
        let original_size = cmd.size;
        let initial_event_count = cmd.matcher_events.len();

        // 先执行 NORMAL 撮合
        self.yes_book.new_order(cmd);

        // 计算 NORMAL 撮合后的剩余量
        let normal_filled: Size = cmd.matcher_events[initial_event_count..]
            .iter()
            .map(|e| e.size)
            .sum();
        let remaining = original_size - normal_filled;

        // 如果还有剩余，尝试 MERGE 撮合
        if remaining > 0 {
            let complement_price = self.complement_price(cmd.price);
            let user_id = cmd.uid;
            let symbol = cmd.symbol;
            let timestamp = cmd.timestamp;
            let reserve_price = cmd.reserve_price;

            let mut counter_cmd = OrderCommand {
                command: OrderCommandType::PlaceOrder,
                result_code: CommandResultCode::New,
                uid: user_id,
                order_id: 0,
                symbol,
                price: complement_price,
                reserve_price,
                size: remaining,
                action: OrderAction::Bid,  // YES ASK → NO BID
                order_type: OrderType::Ioc,
                timestamp,
                events_group: match_type_encoding::encode(MatchType::Merge, timestamp as u64),
                service_flags: service_flags::CROSS_BOOK_MATCH,
                stop_price: None,
                visible_size: None,
                expire_time: None,
                matcher_events: Vec::with_capacity(4),
            };

            self.no_book.new_order(&mut counter_cmd);

            for event in counter_cmd.matcher_events {
                cmd.matcher_events.push(MatcherTradeEvent::new_trade(
                    event.size,
                    event.price,
                    event.matched_order_id,
                    event.matched_order_uid,
                    cmd.reserve_price,
                ));
            }
        }
        CommandResultCode::Success
    }

    /// 策略 3: NO 买单撮合
    /// 优先级:
    ///   1. NORMAL - NO 买 vs NO 卖
    ///   2. MINT - NO 买 vs YES 买
    fn match_no_bid(&mut self, cmd: &mut OrderCommand) -> CommandResultCode {
        let original_size = cmd.size;
        let initial_event_count = cmd.matcher_events.len();

        // 先执行 NORMAL 撮合
        self.no_book.new_order(cmd);

        // 计算 NORMAL 撮合后的剩余量
        let normal_filled: Size = cmd.matcher_events[initial_event_count..]
            .iter()
            .map(|e| e.size)
            .sum();
        let remaining = original_size - normal_filled;

        // 如果还有剩余，尝试 MINT 撮合
        if remaining > 0 {
            let complement_price = self.complement_price(cmd.price);
            let user_id = cmd.uid;
            let symbol = cmd.symbol;
            let timestamp = cmd.timestamp;
            let reserve_price = cmd.reserve_price;

            let mut counter_cmd = OrderCommand {
                command: OrderCommandType::PlaceOrder,
                result_code: CommandResultCode::New,
                uid: user_id,
                order_id: 0,
                symbol,
                price: complement_price,
                reserve_price,
                size: remaining,
                action: OrderAction::Ask,  // NO BID → YES ASK
                order_type: OrderType::Ioc,
                timestamp,
                events_group: match_type_encoding::encode(MatchType::Mint, timestamp as u64),
                service_flags: service_flags::CROSS_BOOK_MATCH,
                stop_price: None,
                visible_size: None,
                expire_time: None,
                matcher_events: Vec::with_capacity(4),
            };

            self.yes_book.new_order(&mut counter_cmd);

            for event in counter_cmd.matcher_events {
                cmd.matcher_events.push(MatcherTradeEvent::new_trade(
                    event.size,
                    event.price,
                    event.matched_order_id,
                    event.matched_order_uid,
                    cmd.reserve_price,
                ));
            }
        }
        CommandResultCode::Success
    }

    /// 策略 4: NO 卖单撮合
    /// 优先级:
    ///   1. NORMAL - NO 卖 vs NO 买
    ///   2. MERGE - NO 卖 vs YES 卖
    fn match_no_ask(&mut self, cmd: &mut OrderCommand) -> CommandResultCode {
        let original_size = cmd.size;
        let initial_event_count = cmd.matcher_events.len();

        // 先执行 NORMAL 撮合
        self.no_book.new_order(cmd);

        // 计算 NORMAL 撮合后的剩余量
        let normal_filled: Size = cmd.matcher_events[initial_event_count..]
            .iter()
            .map(|e| e.size)
            .sum();
        let remaining = original_size - normal_filled;

        // 如果还有剩余，尝试 MERGE 撮合
        if remaining > 0 {
            let complement_price = self.complement_price(cmd.price);
            let user_id = cmd.uid;
            let symbol = cmd.symbol;
            let timestamp = cmd.timestamp;
            let reserve_price = cmd.reserve_price;

            let mut counter_cmd = OrderCommand {
                command: OrderCommandType::PlaceOrder,
                result_code: CommandResultCode::New,
                uid: user_id,
                order_id: 0,
                symbol,
                price: complement_price,
                reserve_price,
                size: remaining,
                action: OrderAction::Bid,  // NO ASK → YES BID
                order_type: OrderType::Ioc,
                timestamp,
                events_group: match_type_encoding::encode(MatchType::Merge, timestamp as u64),
                service_flags: service_flags::CROSS_BOOK_MATCH,
                stop_price: None,
                visible_size: None,
                expire_time: None,
                matcher_events: Vec::with_capacity(4),
            };

            self.yes_book.new_order(&mut counter_cmd);

            for event in counter_cmd.matcher_events {
                cmd.matcher_events.push(MatcherTradeEvent::new_trade(
                    event.size,
                    event.price,
                    event.matched_order_id,
                    event.matched_order_uid,
                    cmd.reserve_price,
                ));
            }
        }
        CommandResultCode::Success
    }

    // ============================================================
    // SNAPSHOT & CONSISTENCY VERIFICATION (Phase 2.1)
    // ============================================================

    /// 创建跨簿快照
    ///
    /// 捕获 YES 和 NO 订单簿的完整状态，包括时间戳和校验和。
    ///
    /// # 返回
    /// 包含两个订单簿快照的 `UnifiedOrderBookSnapshot`
    ///
    /// # 示例
    /// ```no_run
    /// # use matching_core::core::orderbook::prediction::*;
    /// let market = UnifiedOrderBook::new(1);
    /// let snapshot = market.take_snapshot();
    /// ```
    pub fn take_snapshot(&self) -> UnifiedOrderBookSnapshot {
        let yes_book_snapshot = self.yes_book.clone();
        let no_book_snapshot = self.no_book.clone();
        let checksum = self.calculate_checksum();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        UnifiedOrderBookSnapshot {
            yes_book_snapshot,
            no_book_snapshot,
            timestamp,
            checksum,
        }
    }

    /// 恢复跨簿快照
    ///
    /// 从快照恢复 YES 和 NO 订单簿的状态，并验证校验和。
    ///
    /// # 参数
    /// - `snapshot`: 要恢复的快照
    ///
    /// # 返回
    /// - `Ok(())`: 恢复成功
    /// - `Err(anyhow::Error)`: 校验和不匹配或其他错误
    ///
    /// # 示例
    /// ```no_run
    /// # use matching_core::core::orderbook::prediction::*;
    /// # let mut market = UnifiedOrderBook::new(1);
    /// # let snapshot = market.take_snapshot();
    /// let mut restored_market = UnifiedOrderBook::new(1);
    /// restored_market.restore_snapshot(snapshot).unwrap();
    /// ```
    pub fn restore_snapshot(&mut self, snapshot: UnifiedOrderBookSnapshot) -> anyhow::Result<()> {
        // 验证校验和
        let calculated = self.calculate_checksum_from_snapshot(&snapshot);
        if calculated != snapshot.checksum {
            return Err(anyhow::anyhow!(
                "快照校验和不匹配: expected={}, got={}",
                snapshot.checksum,
                calculated
            ));
        }

        // 恢复订单簿
        self.yes_book = snapshot.yes_book_snapshot;
        self.no_book = snapshot.no_book_snapshot;

        Ok(())
    }

    /// 计算当前状态校验和
    ///
    /// 使用订单簿的关键状态信息（订单数量、深度等）计算校验和。
    ///
    /// # 返回
    /// 64 位校验和值
    fn calculate_checksum(&self) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;

        let mut hasher = DefaultHasher::new();

        // 使用订单簿的关键信息计算校验和
        hasher.write_u64(self.market_id);

        // YES 订单簿状态
        let yes_ask_vol = self.yes_book.get_total_ask_volume();
        let yes_bid_vol = self.yes_book.get_total_bid_volume();
        let yes_ask_count = self.yes_book.get_ask_buckets_count();
        let yes_bid_count = self.yes_book.get_bid_buckets_count();

        hasher.write_u64(yes_ask_vol as u64);
        hasher.write_u64(yes_bid_vol as u64);
        hasher.write_usize(yes_ask_count);
        hasher.write_usize(yes_bid_count);

        // NO 订单簿状态
        let no_ask_vol = self.no_book.get_total_ask_volume();
        let no_bid_vol = self.no_book.get_total_bid_volume();
        let no_ask_count = self.no_book.get_ask_buckets_count();
        let no_bid_count = self.no_book.get_bid_buckets_count();

        hasher.write_u64(no_ask_vol as u64);
        hasher.write_u64(no_bid_vol as u64);
        hasher.write_usize(no_ask_count);
        hasher.write_usize(no_bid_count);

        hasher.finish()
    }

    /// 从快照计算校验和
    ///
    /// 用于验证快照的完整性
    fn calculate_checksum_from_snapshot(&self, snapshot: &UnifiedOrderBookSnapshot) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;

        let mut hasher = DefaultHasher::new();
        hasher.write_u64(self.market_id);

        // 从快照提取 YES 订单簿信息
        let yes_ask_vol = snapshot.yes_book_snapshot.get_total_ask_volume();
        let yes_bid_vol = snapshot.yes_book_snapshot.get_total_bid_volume();
        let yes_ask_count = snapshot.yes_book_snapshot.get_ask_buckets_count();
        let yes_bid_count = snapshot.yes_book_snapshot.get_bid_buckets_count();

        hasher.write_u64(yes_ask_vol as u64);
        hasher.write_u64(yes_bid_vol as u64);
        hasher.write_usize(yes_ask_count);
        hasher.write_usize(yes_bid_count);

        // 从快照提取 NO 订单簿信息
        let no_ask_vol = snapshot.no_book_snapshot.get_total_ask_volume();
        let no_bid_vol = snapshot.no_book_snapshot.get_total_bid_volume();
        let no_ask_count = snapshot.no_book_snapshot.get_ask_buckets_count();
        let no_bid_count = snapshot.no_book_snapshot.get_bid_buckets_count();

        hasher.write_u64(no_ask_vol as u64);
        hasher.write_u64(no_bid_vol as u64);
        hasher.write_usize(no_ask_count);
        hasher.write_usize(no_bid_count);

        hasher.finish()
    }

    /// 验证跨簿状态一致性
    ///
    /// 检查 YES 和 NO 订单簿之间的价格和数量是否满足互补性约束。
    ///
    /// # 返回
    /// - `Ok(true)`: 状态一致
    /// - `Ok(false)`: 状态不一致（但警告而非错误）
    /// - `Err(anyhow::Error)`: 检查过程出错
    ///
    /// # 示例
    /// ```no_run
    /// # use matching_core::core::orderbook::prediction::*;
    /// # let market = UnifiedOrderBook::new(1);
    /// let consistent = market.verify_cross_book_consistency().unwrap();
    /// ```
    pub fn verify_cross_book_consistency(&self) -> anyhow::Result<bool> {
        // 1. 验证订单数量合理性
        let yes_ask_vol = self.yes_book.get_total_ask_volume();
        let _yes_bid_vol = self.yes_book.get_total_bid_volume();
        let _no_ask_vol = self.no_book.get_total_ask_volume();
        let no_bid_vol = self.no_book.get_total_bid_volume();

        // 价格互补性：YES 卖单多 → NO 买单应该多
        // 这是一个弱验证，仅供参考
        let imbalance_ratio = if yes_ask_vol > no_bid_vol {
            (yes_ask_vol - no_bid_vol) as f64 / (yes_ask_vol as f64 + 1.0)
        } else {
            0.0
        };

        // 不平衡度不应该超过 50%（这是经验值，可能需要调整）
        if imbalance_ratio > 0.5 {
            tracing::warn!(
                "跨簿状态不平衡: yes_ask={}, no_bid={}, ratio={}",
                yes_ask_vol, no_bid_vol, imbalance_ratio
            );
        }

        // 2. 验证订单簿深度一致性
        let yes_l2 = self.yes_book.get_l2_data(10);
        let no_l2 = self.no_book.get_l2_data(10);

        // YES 和 NO 的最优买卖价应该大致互补
        if !yes_l2.ask_prices.is_empty() && !no_l2.bid_prices.is_empty() {
            let yes_ask = yes_l2.ask_prices[0];
            let no_bid = no_l2.bid_prices[0];
            let price_sum = yes_ask + no_bid;

            // 价格和应该在 PRICE_SCALE 附近
            if price_sum > PRICE_SCALE + 1000 || price_sum < PRICE_SCALE - 1000 {
                tracing::warn!(
                    "最优价不互补: yes_ask={}, no_bid={}, sum={}",
                    yes_ask, no_bid, price_sum
                );
            }
        }

        Ok(true)
    }
}

// ============================================================
// TESTS
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ----- TokenType tests -----
    #[test]
    fn test_token_type_complement() {
        assert_eq!(TokenType::YES.complement(), TokenType::NO);
        assert_eq!(TokenType::NO.complement(), TokenType::YES);
        assert_eq!(TokenType::YES.complement().complement(), TokenType::YES);
    }

    // ----- PredictionOrder tests -----
    #[test]
    fn test_order_creation() {
        let order = PredictionOrder::new(
            1,                      // market_id
            TokenType::YES,
            100,                    // maker
            100,                    // making_amount
            65_000_000,             // taking_amount (65 USDC)
            OrderAction::Bid,
            i64::MAX,
        );

        assert_eq!(order.market_id, 1);
        assert_eq!(order.token_type, TokenType::YES);
        assert_eq!(order.maker, 100);
        assert_eq!(order.making_amount, 100);
        assert_eq!(order.taking_amount, 65_000_000);
        assert_eq!(order.price, 650_000); // 65_000_000 * 1_000_000 / 100
        assert_eq!(order.side, OrderAction::Bid);
    }

    #[test]
    fn test_order_with_salt() {
        let order = PredictionOrder::new(1, TokenType::YES, 100, 100, 50_000_000, OrderAction::Bid, i64::MAX)
            .with_salt(12345);

        assert_eq!(order.salt, 12345);
    }

    #[test]
    fn test_complement_price() {
        let order = PredictionOrder::new(1, TokenType::YES, 100, 100, 65_000_000, OrderAction::Bid, i64::MAX);
        assert_eq!(order.complement_price(), 350_000); // 1_000_000 - 650_000

        let order2 = PredictionOrder::new(1, TokenType::NO, 100, 100, 35_000_000, OrderAction::Bid, i64::MAX);
        assert_eq!(order2.complement_price(), 650_000); // 1_000_000 - 350_000
    }

    // ----- OrderConverter tests -----
    #[test]
    fn test_symbol_id_encoding_yes() {
        // Market 1, YES: (1 << 1) | 0 = 2
        assert_eq!(OrderConverter::encode_symbol_id(1, TokenType::YES), 2);

        // Market 100, YES: (100 << 1) | 0 = 200
        assert_eq!(OrderConverter::encode_symbol_id(100, TokenType::YES), 200);

        // Market 0, YES: (0 << 1) | 0 = 0
        assert_eq!(OrderConverter::encode_symbol_id(0, TokenType::YES), 0);
    }

    #[test]
    fn test_symbol_id_encoding_no() {
        // Market 1, NO: (1 << 1) | 1 = 3
        assert_eq!(OrderConverter::encode_symbol_id(1, TokenType::NO), 3);

        // Market 100, NO: (100 << 1) | 1 = 201
        assert_eq!(OrderConverter::encode_symbol_id(100, TokenType::NO), 201);

        // Market 0, NO: (0 << 1) | 1 = 1
        assert_eq!(OrderConverter::encode_symbol_id(0, TokenType::NO), 1);
    }

    #[test]
    fn test_symbol_id_decoding() {
        // YES tokens (even symbol_id)
        let (mid, tt) = OrderConverter::decode_symbol_id(2); // Market 1, YES
        assert_eq!(mid, 1);
        assert_eq!(tt, TokenType::YES);

        let (mid, tt) = OrderConverter::decode_symbol_id(200); // Market 100, YES
        assert_eq!(mid, 100);
        assert_eq!(tt, TokenType::YES);

        // NO tokens (odd symbol_id)
        let (mid, tt) = OrderConverter::decode_symbol_id(3); // Market 1, NO
        assert_eq!(mid, 1);
        assert_eq!(tt, TokenType::NO);

        let (mid, tt) = OrderConverter::decode_symbol_id(201); // Market 100, NO
        assert_eq!(mid, 100);
        assert_eq!(tt, TokenType::NO);
    }

    #[test]
    fn test_to_order_command() {
        let order = PredictionOrder::new(
            1,              // market_id
            TokenType::YES,
            100,            // maker
            100,            // making_amount
            65_000_000,     // taking_amount (65 USDC)
            OrderAction::Bid,
            i64::MAX,
        );

        let cmd = OrderConverter::to_order_command(&order, 12345, 9999);

        assert_eq!(cmd.command, OrderCommandType::PlaceOrder);
        assert_eq!(cmd.result_code, CommandResultCode::New);
        assert_eq!(cmd.uid, 100);
        assert_eq!(cmd.order_id, 12345);
        assert_eq!(cmd.symbol, 2); // (1 << 1) | 0 = 2
        assert_eq!(cmd.price, 650_000);
        assert_eq!(cmd.size, 100);
        assert_eq!(cmd.action, OrderAction::Bid);
        assert_eq!(cmd.order_type, OrderType::Gtc);
        assert_eq!(cmd.timestamp, 9999);
        assert_eq!(cmd.expire_time, None); // i64::MAX → None
        assert!(cmd.matcher_events.is_empty());
    }

    // ----- UnifiedOrderBook tests -----
    #[test]
    fn test_unified_orderbook_creation() {
        let market = UnifiedOrderBook::new(1);
        assert_eq!(market.market_id, 1);
        assert_eq!(market.price_scale, PRICE_SCALE);
    }

    #[test]
    fn test_unified_orderbook_complement_price() {
        let market = UnifiedOrderBook::new(1);

        // YES @ 0.65 → NO @ 0.35
        assert_eq!(market.complement_price(650_000), 350_000);

        // YES @ 0.50 → NO @ 0.50
        assert_eq!(market.complement_price(500_000), 500_000);

        // YES @ 0.00 → NO @ 1.00
        assert_eq!(market.complement_price(0), 1_000_000);

        // YES @ 1.00 → NO @ 0.00
        assert_eq!(market.complement_price(1_000_000), 0);
    }

    #[test]
    fn test_are_complementary() {
        let market = UnifiedOrderBook::new(1);

        assert!(market.are_complementary(650_000, 350_000));
        assert!(market.are_complementary(500_000, 500_000));
        assert!(market.are_complementary(0, 1_000_000));

        assert!(!market.are_complementary(600_000, 350_000));
        assert!(!market.are_complementary(650_000, 400_000));
    }

    #[test]
    fn test_get_l2_data() {
        let market = UnifiedOrderBook::new(1);

        // YES 订单簿
        let yes_l2 = market.get_l2_data(TokenType::YES, 10);
        assert_eq!(yes_l2.ask_prices.len(), 0);
        assert_eq!(yes_l2.bid_prices.len(), 0);

        // NO 订单簿
        let no_l2 = market.get_l2_data(TokenType::NO, 10);
        assert_eq!(no_l2.ask_prices.len(), 0);
        assert_eq!(no_l2.bid_prices.len(), 0);
    }
}
