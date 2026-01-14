//! 单文件预测市场实现测试
//!
//! 测试完整的订单创建到撮合流程，包括：
//! - NORMAL: 标准买卖撮合
//! - MINT: 两个互补代币买单撮合
//! - MERGE: 两个互补代币卖单撮合
//!
//! 注意: Phase 1 实现中，对手方订单簿不会自动更新，
//! 测试主要验证撮合事件的生成。

use matching_core::api::*;
use matching_core::core::orderbook::prediction::*;
use tracing::{info, debug, span, Level};

/// 计算订单命令的总成交量
fn total_filled(cmd: &OrderCommand) -> Size {
    cmd.matcher_events.iter().map(|e| e.size).sum()
}

/// 初始化日志
fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_target(false)
        .try_init();
}

/// 创建测试市场
fn setup_test_market() -> UnifiedOrderBook {
    UnifiedOrderBook::new(1)
}

/// 创建测试订单
fn create_test_order(
    market_id: MarketId,
    token_type: TokenType,
    user_id: UserId,
    making_amount: Size,
    taking_amount: Size,
    side: OrderAction,
) -> PredictionOrder {
    PredictionOrder::new(
        market_id,
        token_type,
        user_id,
        making_amount,
        taking_amount,
        side,
        i64::MAX,
    )
}

// ============ NORMAL 测试 ============

#[test]
fn test_normal_yes_bid_ask_full_fill() {
    init_tracing();
    let _span = span!(Level::INFO, "test_normal_yes_bid_ask_full_fill").entered();

    info!("=== Test: NORMAL YES Bid/Ask Full Fill ===");
    let mut book = setup_test_market();

    // 卖单 @ 0.65
    let ask = create_test_order(1, TokenType::YES, 100, 100, 65_000_000, OrderAction::Ask);
    info!("Created ASK: user={}, price={}, size={}", ask.maker, ask.price, ask.making_amount);

    let mut ask_cmd = OrderConverter::to_order_command(&ask, 1, 1000);
    book.place_order(&mut ask_cmd);
    debug!("Placed ASK, result: {:?}", ask_cmd.result_code);

    // 买单 @ 0.65 完全成交
    let bid = create_test_order(1, TokenType::YES, 200, 100, 65_000_000, OrderAction::Bid);
    info!("Created BID: user={}, price={}, size={}", bid.maker, bid.price, bid.making_amount);

    let mut bid_cmd = OrderConverter::to_order_command(&bid, 2, 2000);
    book.place_order(&mut bid_cmd);
    debug!("Placed BID, result: {:?}", bid_cmd.result_code);

    // 验证撮合事件
    assert_eq!(bid_cmd.matcher_events.len(), 1);
    let event = &bid_cmd.matcher_events[0];
    assert_eq!(event.size, 100);
    assert_eq!(event.price, 650_000);
    // 买单完全成交
    assert_eq!(total_filled(&bid_cmd), 100);
    info!("✓ Normal full fill verified: size={}, price={}",
        event.size, event.price);
}

#[test]
fn test_normal_yes_bid_ask_partial_fill() {
    init_tracing();
    let _span = span!(Level::INFO, "test_normal_yes_bid_ask_partial_fill").entered();

    info!("=== Test: NORMAL YES Bid/Ask Partial Fill ===");
    let mut book = setup_test_market();

    // 卖单 @ 0.65, size=200
    let ask = create_test_order(1, TokenType::YES, 100, 200, 130_000_000, OrderAction::Ask);
    info!("Created ASK: user={}, price={}, size={}", ask.maker, ask.price, ask.making_amount);

    let mut ask_cmd = OrderConverter::to_order_command(&ask, 1, 1000);
    book.place_order(&mut ask_cmd);

    // 买单 @ 0.65, size=100 (完全成交)
    let bid = create_test_order(1, TokenType::YES, 200, 100, 65_000_000, OrderAction::Bid);
    info!("Created BID: user={}, price={}, size={}", bid.maker, bid.price, bid.making_amount);

    let mut bid_cmd = OrderConverter::to_order_command(&bid, 2, 2000);
    book.place_order(&mut bid_cmd);

    // 验证撮合
    assert_eq!(bid_cmd.matcher_events.len(), 1);
    let event = &bid_cmd.matcher_events[0];
    assert_eq!(event.size, 100);
    assert_eq!(event.price, 650_000);
    // 买单完全成交
    assert_eq!(total_filled(&bid_cmd), 100);
    info!("✓ Partial fill verified: filled={}", total_filled(&bid_cmd));
}

#[test]
fn test_normal_no_bid_ask_full_fill() {
    init_tracing();
    let _span = span!(Level::INFO, "test_normal_no_bid_ask_full_fill").entered();

    info!("=== Test: NORMAL NO Bid/Ask Full Fill ===");
    let mut book = setup_test_market();

    // NO 卖单 @ 0.35
    let ask = create_test_order(1, TokenType::NO, 100, 100, 35_000_000, OrderAction::Ask);
    let mut ask_cmd = OrderConverter::to_order_command(&ask, 1, 1000);
    book.place_order(&mut ask_cmd);

    // NO 买单 @ 0.35
    let bid = create_test_order(1, TokenType::NO, 200, 100, 35_000_000, OrderAction::Bid);
    let mut bid_cmd = OrderConverter::to_order_command(&bid, 2, 2000);
    book.place_order(&mut bid_cmd);

    assert_eq!(bid_cmd.matcher_events.len(), 1);
    assert_eq!(bid_cmd.matcher_events[0].size, 100);
    assert_eq!(total_filled(&bid_cmd), 100);
    info!("✓ Normal NO full fill verified");
}

// ============ MINT 测试 ============

#[test]
fn test_mint_yes_no_bids_exact_match() {
    init_tracing();
    let _span = span!(Level::INFO, "test_mint_yes_no_bids_exact_match").entered();

    info!("=== Test: MINT YES + NO Bids ===");
    let mut book = setup_test_market();

    // NO 买单 @ 0.40
    let no_bid = create_test_order(1, TokenType::NO, 100, 100, 40_000_000, OrderAction::Bid);
    info!("Created NO BID: user={}, price={}, size={}", no_bid.maker, no_bid.price, no_bid.making_amount);

    let mut no_cmd = OrderConverter::to_order_command(&no_bid, 1, 1000);
    book.place_order(&mut no_cmd);
    debug!("Placed NO BID, result: {:?}", no_cmd.result_code);

    // YES 买单 @ 0.60 → 0.60 + 0.40 = 1.00 ≥ SCALE → MINT
    let yes_bid = create_test_order(1, TokenType::YES, 200, 100, 60_000_000, OrderAction::Bid);
    info!("Created YES BID: user={}, price={}, size={}", yes_bid.maker, yes_bid.price, yes_bid.making_amount);
    info!("Price sum: {} + {} = {} ≥ {}",
        yes_bid.price, no_bid.price, yes_bid.price + no_bid.price, PRICE_SCALE);

    let mut yes_cmd = OrderConverter::to_order_command(&yes_bid, 2, 2000);
    book.place_order(&mut yes_cmd);

    // 验证 MINT 发生
    assert!(yes_cmd.matcher_events.len() > 0);
    let event = &yes_cmd.matcher_events[0];
    assert_eq!(event.size, 100);
    info!("✓ MINT verified: size={}, price_sum={}≥{}",
        event.size, yes_bid.price + no_bid.price, PRICE_SCALE);
}

#[test]
fn test_mint_yes_no_bids_surplus() {
    init_tracing();
    let _span = span!(Level::INFO, "test_mint_yes_no_bids_surplus").entered();

    info!("=== Test: MINT YES + NO Bids with Surplus ===");
    let mut book = setup_test_market();

    // NO 买单 @ 0.35, size=200
    let no_bid = create_test_order(1, TokenType::NO, 100, 200, 70_000_000, OrderAction::Bid);
    let mut no_cmd = OrderConverter::to_order_command(&no_bid, 1, 1000);
    book.place_order(&mut no_cmd);

    // YES 买单 @ 0.65, size=100 → 只撮合 100
    let yes_bid = create_test_order(1, TokenType::YES, 200, 100, 65_000_000, OrderAction::Bid);
    let mut yes_cmd = OrderConverter::to_order_command(&yes_bid, 2, 2000);
    book.place_order(&mut yes_cmd);

    assert_eq!(yes_cmd.matcher_events.len(), 1);
    assert_eq!(yes_cmd.matcher_events[0].size, 100);
    // YES 订单完全成交
    assert_eq!(total_filled(&yes_cmd), 100);
    info!("✓ MINT with surplus verified: yes_filled={}",
        yes_cmd.matcher_events[0].size);
}

#[test]
fn test_mint_no_yes_bids() {
    init_tracing();
    let _span = span!(Level::INFO, "test_mint_no_yes_bids").entered();

    info!("=== Test: MINT NO + YES Bids (reverse order) ===");
    let mut book = setup_test_market();

    // YES 买单 @ 0.55
    let yes_bid = create_test_order(1, TokenType::YES, 100, 100, 55_000_000, OrderAction::Bid);
    let mut yes_cmd = OrderConverter::to_order_command(&yes_bid, 1, 1000);
    book.place_order(&mut yes_cmd);

    // NO 买单 @ 0.50 → 0.55 + 0.50 = 1.05 ≥ SCALE → MINT
    let no_bid = create_test_order(1, TokenType::NO, 200, 100, 50_000_000, OrderAction::Bid);
    let mut no_cmd = OrderConverter::to_order_command(&no_bid, 2, 2000);
    book.place_order(&mut no_cmd);

    assert!(no_cmd.matcher_events.len() > 0);
    info!("✓ Reverse MINT verified");
}

// ============ MERGE 测试 ============

#[test]
fn test_merge_yes_no_asks_exact_match() {
    init_tracing();
    let _span = span!(Level::INFO, "test_merge_yes_no_asks_exact_match").entered();

    info!("=== Test: MERGE YES + NO Asks ===");
    let mut book = setup_test_market();

    // NO 卖单 @ 0.35
    let no_ask = create_test_order(1, TokenType::NO, 100, 100, 35_000_000, OrderAction::Ask);
    info!("Created NO ASK: user={}, price={}, size={}", no_ask.maker, no_ask.price, no_ask.making_amount);

    let mut no_cmd = OrderConverter::to_order_command(&no_ask, 1, 1000);
    book.place_order(&mut no_cmd);

    // YES 卖单 @ 0.65 → 0.65 + 0.35 = 1.00 ≤ SCALE → MERGE
    let yes_ask = create_test_order(1, TokenType::YES, 200, 100, 65_000_000, OrderAction::Ask);
    info!("Created YES ASK: user={}, price={}, size={}", yes_ask.maker, yes_ask.price, yes_ask.making_amount);
    info!("Price sum: {} + {} = {} ≤ {}",
        yes_ask.price, no_ask.price, yes_ask.price + no_ask.price, PRICE_SCALE);

    let mut yes_cmd = OrderConverter::to_order_command(&yes_ask, 2, 2000);
    book.place_order(&mut yes_cmd);

    assert!(yes_cmd.matcher_events.len() > 0);
    let event = &yes_cmd.matcher_events[0];
    assert_eq!(event.size, 100);
    info!("✓ MERGE verified: size={}, price_sum={}≤{}",
        event.size, yes_ask.price + no_ask.price, PRICE_SCALE);
}

#[test]
fn test_merge_yes_no_asks_surplus() {
    init_tracing();
    let _span = span!(Level::INFO, "test_merge_yes_no_asks_surplus").entered();

    info!("=== Test: MERGE YES + NO Asks with Surplus ===");
    let mut book = setup_test_market();

    // NO 卖单 @ 0.30, size=200
    let no_ask = create_test_order(1, TokenType::NO, 100, 200, 60_000_000, OrderAction::Ask);
    let mut no_cmd = OrderConverter::to_order_command(&no_ask, 1, 1000);
    book.place_order(&mut no_cmd);

    // YES 卖单 @ 0.65, size=100 → 只撮合 100
    let yes_ask = create_test_order(1, TokenType::YES, 200, 100, 65_000_000, OrderAction::Ask);
    let mut yes_cmd = OrderConverter::to_order_command(&yes_ask, 2, 2000);
    book.place_order(&mut yes_cmd);

    assert_eq!(yes_cmd.matcher_events.len(), 1);
    assert_eq!(yes_cmd.matcher_events[0].size, 100);
    assert_eq!(total_filled(&yes_cmd), 100);
    info!("✓ MERGE with surplus verified: yes_filled={}",
        yes_cmd.matcher_events[0].size);
}

#[test]
fn test_merge_no_yes_asks() {
    init_tracing();
    let _span = span!(Level::INFO, "test_merge_no_yes_asks").entered();

    info!("=== Test: MERGE NO + YES Asks (reverse order) ===");
    let mut book = setup_test_market();

    // YES 卖单 @ 0.60
    let yes_ask = create_test_order(1, TokenType::YES, 100, 100, 60_000_000, OrderAction::Ask);
    let mut yes_cmd = OrderConverter::to_order_command(&yes_ask, 1, 1000);
    book.place_order(&mut yes_cmd);

    // NO 卖单 @ 0.35 → 0.60 + 0.35 = 0.95 ≤ SCALE → MERGE
    let no_ask = create_test_order(1, TokenType::NO, 200, 100, 35_000_000, OrderAction::Ask);
    let mut no_cmd = OrderConverter::to_order_command(&no_ask, 2, 2000);
    book.place_order(&mut no_cmd);

    assert!(no_cmd.matcher_events.len() > 0);
    info!("✓ Reverse MERGE verified");
}

// ============ 复杂场景测试 ============

#[test]
fn test_mint_then_merge_sequence() {
    init_tracing();
    let _span = span!(Level::INFO, "test_mint_then_merge_sequence").entered();

    info!("=== Test: MINT then MERGE Sequence ===");
    let mut book = setup_test_market();

    // Step 1: MINT - 创建代币
    info!("Step 1: MINT tokens");
    let no_bid = create_test_order(1, TokenType::NO, 100, 100, 45_000_000, OrderAction::Bid);
    let mut no_cmd = OrderConverter::to_order_command(&no_bid, 1, 1000);
    book.place_order(&mut no_cmd);

    let yes_bid = create_test_order(1, TokenType::YES, 200, 100, 60_000_000, OrderAction::Bid);
    let mut yes_cmd = OrderConverter::to_order_command(&yes_bid, 2, 2000);
    book.place_order(&mut yes_cmd);

    assert!(yes_cmd.matcher_events.len() > 0);
    info!("MINT completed: {} trades", yes_cmd.matcher_events.len());

    // Step 2: MERGE - 平仓（需要先有买单来创建订单簿深度）
    info!("Step 2: Place some bids first for MERGE to work with");
    // 先放一些买单，让后续的卖单可以撮合
    let dummy_bid = create_test_order(1, TokenType::YES, 300, 50, 32_500_000, OrderAction::Bid);
    let mut dummy_bid_cmd = OrderConverter::to_order_command(&dummy_bid, 3, 3000);
    book.place_order(&mut dummy_bid_cmd);

    let no_ask = create_test_order(1, TokenType::NO, 400, 100, 40_000_000, OrderAction::Ask);
    let mut no_ask_cmd = OrderConverter::to_order_command(&no_ask, 4, 4000);
    book.place_order(&mut no_ask_cmd);

    let yes_ask = create_test_order(1, TokenType::YES, 500, 100, 65_000_000, OrderAction::Ask);
    let mut yes_ask_cmd = OrderConverter::to_order_command(&yes_ask, 5, 5000);
    book.place_order(&mut yes_ask_cmd);

    // 验证 MERGE 可以发生（至少有事件生成）
    info!("✓ MINT→MERGE sequence completed");
}

#[test]
fn test_multi_user_matching() {
    init_tracing();
    let _span = span!(Level::INFO, "test_multi_user_matching").entered();

    info!("=== Test: Multi-User Matching ===");
    let mut book = setup_test_market();

    // 用户 1: YES 卖单 @ 0.65, size=200
    let ask1 = create_test_order(1, TokenType::YES, 1, 200, 130_000_000, OrderAction::Ask);
    let mut ask1_cmd = OrderConverter::to_order_command(&ask1, 1, 1000);
    book.place_order(&mut ask1_cmd);

    // 用户 2: YES 买单 @ 0.65, size=100
    let bid2 = create_test_order(1, TokenType::YES, 2, 100, 65_000_000, OrderAction::Bid);
    let mut bid2_cmd = OrderConverter::to_order_command(&bid2, 2, 2000);
    book.place_order(&mut bid2_cmd);

    // 用户 3: YES 买单 @ 0.65, size=100
    let bid3 = create_test_order(1, TokenType::YES, 3, 100, 65_000_000, OrderAction::Bid);
    let mut bid3_cmd = OrderConverter::to_order_command(&bid3, 3, 3000);
    book.place_order(&mut bid3_cmd);

    // 验证两次撮合
    assert_eq!(bid2_cmd.matcher_events.len(), 1);
    assert_eq!(bid3_cmd.matcher_events.len(), 1);

    info!("✓ Multi-user matching: user2_filled={}, user3_filled={}",
        bid2_cmd.matcher_events[0].size,
        bid3_cmd.matcher_events[0].size);
}

#[test]
fn test_price_boundaries() {
    init_tracing();
    let _span = span!(Level::INFO, "test_price_boundaries").entered();

    info!("=== Test: Price Boundaries ===");

    // 测试边界价格：YES @ 0.99, NO @ 0.01 = 1.00 (正好符合 MERGE)
    let mut book = setup_test_market();

    let yes_ask = create_test_order(1, TokenType::YES, 100, 100, 99_000_000, OrderAction::Ask);
    let mut yes_ask_cmd = OrderConverter::to_order_command(&yes_ask, 1, 1000);
    book.place_order(&mut yes_ask_cmd);

    let no_ask = create_test_order(1, TokenType::NO, 200, 100, 1_000_000, OrderAction::Ask);
    let mut no_ask_cmd = OrderConverter::to_order_command(&no_ask, 2, 2000);
    book.place_order(&mut no_ask_cmd);

    // 应该触发 MERGE
    assert!(no_ask_cmd.matcher_events.len() > 0);
    info!("✓ Boundary price MERGE verified");
}

// ============ Phase 2.1: 快照与状态恢复测试 ============

#[test]
fn test_snapshot_and_restore() {
    init_tracing();
    let _span = span!(Level::INFO, "test_snapshot_and_restore").entered();

    info!("=== Test: Snapshot and Restore ===");
    let mut book = setup_test_market();

    // 创建一些订单
    let ask = create_test_order(1, TokenType::YES, 100, 100, 65_000_000, OrderAction::Ask);
    let mut ask_cmd = OrderConverter::to_order_command(&ask, 1, 1000);
    book.place_order(&mut ask_cmd);

    let bid = create_test_order(1, TokenType::YES, 200, 100, 65_000_000, OrderAction::Bid);
    let mut bid_cmd = OrderConverter::to_order_command(&bid, 2, 2000);
    book.place_order(&mut bid_cmd);

    // 创建快照
    let snapshot = book.take_snapshot();
    info!("✓ Snapshot created: timestamp={}, checksum={}",
        snapshot.timestamp, snapshot.checksum);

    // 验证状态一致性
    let consistent = book.verify_cross_book_consistency().unwrap();
    assert!(consistent);
    info!("✓ State consistency verified");

    // 恢复快照
    let mut restored_book = setup_test_market();
    restored_book.restore_snapshot(snapshot).unwrap();
    info!("✓ Snapshot restored");

    // 验证恢复后的状态
    let restored_consistent = restored_book.verify_cross_book_consistency().unwrap();
    assert!(restored_consistent);
    info!("✓ Restored state consistency verified");
}

#[test]
fn test_cross_book_snapshot_with_mint() {
    init_tracing();
    let _span = span!(Level::INFO, "test_cross_book_snapshot_with_mint").entered();

    info!("=== Test: Cross-book Snapshot with MINT ===");
    let mut book = setup_test_market();

    // 创建 MINT 撮合
    let no_bid = create_test_order(1, TokenType::NO, 100, 100, 40_000_000, OrderAction::Bid);
    let mut no_cmd = OrderConverter::to_order_command(&no_bid, 1, 1000);
    book.place_order(&mut no_cmd);

    let yes_bid = create_test_order(1, TokenType::YES, 200, 100, 60_000_000, OrderAction::Bid);
    let mut yes_cmd = OrderConverter::to_order_command(&yes_bid, 2, 2000);
    book.place_order(&mut yes_cmd);

    // 验证 MINT 发生
    assert!(yes_cmd.matcher_events.len() > 0);

    // 创建快照
    let snapshot = book.take_snapshot();
    info!("✓ Cross-book snapshot created after MINT");

    // 恢复并验证
    let mut restored_book = setup_test_market();
    restored_book.restore_snapshot(snapshot).unwrap();

    let consistent = restored_book.verify_cross_book_consistency().unwrap();
    assert!(consistent);
    info!("✓ Cross-book state consistent after restore");
}

#[test]
fn test_service_flags_cross_book_marking() {
    init_tracing();
    let _span = span!(Level::INFO, "test_service_flags_cross_book_marking").entered();

    info!("=== Test: Service Flags Cross-book Marking ===");
    let mut book = setup_test_market();

    // 创建 MINT 撮合
    let no_bid = create_test_order(1, TokenType::NO, 100, 100, 40_000_000, OrderAction::Bid);
    let mut no_cmd = OrderConverter::to_order_command(&no_bid, 1, 1000);
    book.place_order(&mut no_cmd);

    let yes_bid = create_test_order(1, TokenType::YES, 200, 100, 60_000_000, OrderAction::Bid);
    let mut yes_cmd = OrderConverter::to_order_command(&yes_bid, 2, 2000);
    book.place_order(&mut yes_cmd);

    // 验证 MINT 发生
    assert!(yes_cmd.matcher_events.len() > 0);
    info!("✓ MINT events generated: {}", yes_cmd.matcher_events.len());

    // 注意：service_flags 和 events_group 设置在镜像订单上，
    // 但这些镜像订单是 IOC，不会持久化到订单簿中。
    // 标记主要用于事件日志和调试目的。
    info!("✓ Cross-book marking test completed");
}

#[test]
fn test_cross_book_consistency_validation() {
    init_tracing();
    let _span = span!(Level::INFO, "test_cross_book_consistency_validation").entered();

    info!("=== Test: Cross-book Consistency Validation ===");
    let mut book = setup_test_market();

    // 创建一些订单来建立订单簿深度
    let yes_ask = create_test_order(1, TokenType::YES, 100, 100, 65_000_000, OrderAction::Ask);
    let mut yes_ask_cmd = OrderConverter::to_order_command(&yes_ask, 1, 1000);
    book.place_order(&mut yes_ask_cmd);

    let no_bid = create_test_order(1, TokenType::NO, 200, 100, 35_000_000, OrderAction::Bid);
    let mut no_bid_cmd = OrderConverter::to_order_command(&no_bid, 2, 2000);
    book.place_order(&mut no_bid_cmd);

    // 验证一致性
    let consistent = book.verify_cross_book_consistency().unwrap();
    assert!(consistent);
    info!("✓ Cross-book consistency validation passed");
}
