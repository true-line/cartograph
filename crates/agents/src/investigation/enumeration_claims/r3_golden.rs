//! Exact admitted DEV r3 fields and cited bytes. AC-0287 / #1698.
use super::*;
#[test]
fn all_r3_goldens_against_exact_cited_spans() {
    let evidence_1 = (
        r###"packages/core/src/config/order/default-order-process.ts"###,
        r###"transitions: {
            Created: {
                to: ['AddingItems', 'Draft'],
            },
            Draft: {
                to: ['Cancelled', 'ArrangingPayment'],
            },
            AddingItems: {
                to: ['ArrangingPayment', 'Cancelled'],
            },
            ArrangingPayment: {
                to: ['PaymentAuthorized', 'PaymentSettled', 'AddingItems', 'Cancelled'],
            },
            PaymentAuthorized: {
                to: ['PaymentSettled', 'Cancelled', 'Modifying', 'ArrangingAdditionalPayment'],
            },
            PaymentSettled: {
                to: [
                    'PartiallyDelivered',
                    'Delivered',
                    'PartiallyShipped',
                    'Shipped',
                    'Cancelled',
                    'Modifying',
                    'ArrangingAdditionalPayment',
                ],
            },
            PartiallyShipped: {
                to: ['Shipped', 'PartiallyDelivered', 'Cancelled', 'Modifying'],
            },
            Shipped: {
                to: ['PartiallyDelivered', 'Delivered', 'Cancelled', 'Modifying'],
            },
            PartiallyDelivered: {
                to: ['Delivered', 'Cancelled', 'Modifying'],
            },
            Delivered: {
                to: ['Cancelled'],
            },
            Modifying: {
                to: [
                    'PaymentAuthorized',
                    'PaymentSettled',
                    'PartiallyShipped',
                    'Shipped',
                    'PartiallyDelivered',
                    'ArrangingAdditionalPayment',
                ],
            },
            ArrangingAdditionalPayment: {
                to: [
                    'PaymentAuthorized',
                    'PaymentSettled',
                    'PartiallyShipped',
                    'Shipped',
                    'PartiallyDelivered',
                    'Cancelled',
                ],
            },
            Cancelled: {
                to: [],
            },
        }"###,
    );
    let evidence_2 = (
        r###"packages/core/src/config/order/default-stock-allocation-strategy.ts"###,
        r###"shouldAllocateStock(
        ctx: RequestContext,
        fromState: OrderState,
        toState: OrderState,
        order: Order,
    ): boolean | Promise<boolean> {
        return (
            fromState === 'ArrangingPayment' &&
            (toState === 'PaymentAuthorized' || toState === 'PaymentSettled')
        );
    }"###,
    );
    let evidence_3 = (
        r###"packages/core/src/service/services/order.service.ts"###,
        r###"async addItemToOrder(
        ctx: RequestContext,
        orderId: ID,
        productVariantId: ID,
        quantity: number,
        customFields?: { [key: string]: any },
        relations?: RelationPaths<Order>,
    ): Promise<ErrorResultUnion<UpdateOrderItemsResult, Order>> {
        const result = await this.addItemsToOrder(
            ctx,
            orderId,
            [{ productVariantId, quantity, customFields }],
            relations,
        );
        if (result.errorResults.length) {
            return result.errorResults[0];
        } else {
            return result.order;
        }
    }"###,
    );
    let evidence_4 = (
        r###"packages/core/src/service/services/stock-level.service.ts"###,
        r###"async getStockLevel(ctx: RequestContext, productVariantId: ID, stockLocationId: ID): Promise<StockLevel> {
        const stockLevel = await this.connection.getRepository(ctx, StockLevel).findOne({
            where: {
                productVariantId,
                stockLocationId,
            },
        });
        if (stockLevel) {
            return stockLevel;
        }
        return this.connection.getRepository(ctx, StockLevel).save(
            new StockLevel({
                productVariantId,
                stockLocationId,
                stockOnHand: 0,
                stockAllocated: 0,
            }),
        );
    }"###,
    );
    let evidence_5 = (
        r###"packages/core/src/config/catalog/multi-channel-stock-location-strategy.ts"###,
        r###"async forAllocation(
        ctx: RequestContext,
        stockLocations: StockLocation[],
        orderLine: OrderLine,
        quantity: number,
    ): Promise<LocationWithQuantity[]> {
        const stockLevels = await this.getStockLevelsForVariant(ctx, orderLine.productVariantId);
        const variant = await this.connection.getEntityOrThrow(
            ctx,
            ProductVariant,
            orderLine.productVariantId,
            { loadEagerRelations: false },
        );
        let totalAllocated = 0;
        const locations: LocationWithQuantity[] = [];
        const { inventoryNotTracked, effectiveOutOfStockThreshold } = await this.getVariantStockSettings(
            ctx,
            variant,
        );
        for (const stockLocation of stockLocations) {
            const stockLevel = stockLevels.find(sl => sl.stockLocationId === stockLocation.id);
            if (stockLevel && (await this.stockLevelAppliesToActiveChannel(ctx, stockLevel))) {
                const quantityAvailable = inventoryNotTracked
                    ? Number.MAX_SAFE_INTEGER
                    : stockLevel.stockOnHand - stockLevel.stockAllocated - effectiveOutOfStockThreshold;
                if (quantityAvailable > 0) {
                    const quantityToAllocate = Math.min(quantity, quantityAvailable);
                    locations.push({
                        location: stockLocation,
                        quantity: quantityToAllocate,
                    });
                    totalAllocated += quantityToAllocate;
                }
            }
            if (totalAllocated >= quantity) {
                break;
            }
        }
        return locations;
    }"###,
    );
    let evidence_6 = (
        r###"packages/core/src/service/services/order.service.ts"###,
        r###"async transitionToState(
        ctx: RequestContext,
        orderId: ID,
        state: OrderState,
    ): Promise<Order | OrderStateTransitionError> {
        // Wrapped in withTransaction so that the in-memory state mutation, the
        // first save, the onTransitionEnd hooks (which themselves perform DB
        // writes — stock allocations, history entries, OrderPlacedStrategy
        // mutations) and the second save all commit or roll back together.
        // Without this, a throwing onTransitionEnd would leave the order in a
        // half-committed state (state=new, active=true, orderPlacedAt=null).
        // Joins any existing transaction in ctx, so callers that already have
        // @Transaction() are unaffected. See #4686.
        return this.connection.withTransaction(ctx, async txCtx => {
            const order = await this.getOrderOrThrow(txCtx, orderId);
            order.payments = await this.getOrderPayments(txCtx, orderId);
            const fromState = order.state;
            let finalize: () => Promise<any>;
            try {
                const result = await this.orderStateMachine.transition(txCtx, order, state);
                finalize = result.finalize;
            } catch (e: any) {
                const transitionError = txCtx.translate(e.message, { fromState, toState: state });
                return new OrderStateTransitionError({ transitionError, fromState, toState: state });
            }
            await this.connection.getRepository(txCtx, Order).save(order, { reload: false });
            await this.eventBus.publish(new OrderStateTransitionEvent(fromState, state, txCtx, order));
            await finalize();
            await this.connection.getRepository(txCtx, Order).save(order, { reload: false });
            return order;
        });
    }"###,
    );
    let evidence_7 = (
        r###"packages/core/src/service/services/stock-location.service.ts"###,
        r###"async getAllocationLocations(ctx: RequestContext, orderLine: OrderLine, quantity: number) {
        const { stockLocationStrategy } = this.configService.catalogOptions;
        const stockLocations = await this.getAllStockLocations(ctx);
        const allocationLocations = await stockLocationStrategy.forAllocation(
            ctx,
            stockLocations,
            orderLine,
            quantity,
        );
        return allocationLocations;
    }"###,
    );
    let spans = [evidence_1];
    let title = r###"Default order process defines a state graph from Created through delivery and cancellation"###;
    let statement = r###"The supplied default-order-process.ts declares a transition table with a Created entry that permits moving to AddingItems or Draft, and the table chains through payment, shipping, delivery, modification and cancellation states. Each state entry lists only the target states it allows, and Cancelled permits no further transitions."###;
    assert_eq!(finding(title, statement, &spans), None, "finding-1");
    let spans = [evidence_3];
    let title = r###"addItemToOrder delegates item addition to addItemsToOrder and surfaces only the first error"###;
    let statement = r###"In the supplied order.service.ts, addItemToOrder wraps a single productVariantId and quantity into one item and delegates to addItemsToOrder; it then returns the first error result if any error results exist, otherwise the updated order."###;
    assert_eq!(finding(title, statement, &spans), None, "finding-2");
    let spans = [evidence_6];
    let title = r###"Order state transitions are wrapped in one transaction that includes stock allocation hooks"###;
    let statement = r###"The supplied transitionToState body runs inside a single database transaction: it loads the order and its payments, calls orderStateMachine.transition, saves the order, publishes an OrderStateTransitionEvent, invokes finalize, and saves again. The inline comment states that finalize hooks perform database writes including stock allocations, history entries and OrderPlacedStrategy mutations, and that the transaction prevents a throwing onTransitionEnd hook from leaving a half-committed order. Transition failures return an OrderStateTransitionError instead of throwing."###;
    assert_eq!(finding(title, statement, &spans), None, "finding-3");
    let spans = [evidence_2];
    let title = r###"Default stock allocation strategy allocates only on transition into payment-authorized or payment-settled states"###;
    let statement = r###"The supplied default-stock-allocation-strategy.ts implements shouldAllocateStock by returning true exactly when the order moves from ArrangingPayment into PaymentAuthorized or PaymentSettled, and false for all other transitions."###;
    assert_eq!(finding(title, statement, &spans), None, "finding-4");
    let spans = [evidence_7, evidence_5];
    let title = r###"Allocation locations are resolved via the configured stock-location strategy, defaulting to multi-channel allocation"###;
    let statement = r###"StockLocationService.getAllocationLocations reads the configured catalog stockLocationStrategy and calls its forAllocation with all stock locations, the order line and quantity. The supplied MultiChannelStockLocationStrategy.forAllocation iterates the stock locations, computes per-location available quantity as stockOnHand minus stockAllocated minus an effective out-of-stock threshold (or an unbounded maximum when inventory is not tracked), allocates the minimum of requested and available quantity per location, and stops once the total allocation meets the requested quantity."###;
    assert_eq!(
        finding(title, statement, &spans),
        Some((true, 1, Reason::Configuration))
    );
    assert_eq!(finding("", statement, &spans), None, "finding-5 body");
    let spans = [evidence_4];
    let title = r###"Stock levels are lazily created with zero on-hand and zero allocated"###;
    let statement = r###"The supplied stock-level.service.ts getStockLevel looks up a StockLevel row by productVariantId and stockLocationId, and when none exists it creates and saves a new StockLevel with stockOnHand and stockAllocated both initialized to zero."###;
    assert_eq!(finding(title, statement, &spans), None, "finding-6");
    let spans = [evidence_6, evidence_2];
    let title = r###"Stock allocation is tied to the payment transition, not to adding items"###;
    let statement = r###"The transition pipeline's finalize hooks include stock allocations, and the default allocation strategy gates on the ArrangingPayment-to-PaymentAuthorized/PaymentSettled transition; together these supplied spans indicate that stock allocation is wired into the order state machine's transition finalization rather than performed as a separate step at item-addition time."###;
    assert_eq!(finding(title, statement, &spans), None, "finding-7");
    let spans = [evidence_6];
    let title = r###"Half-committed transition states motivated the transactional design"###;
    let statement = r###"The inline comment in transitionToState explicitly names the failure mode this design guards against: without a shared transaction, a throwing onTransitionEnd hook would leave the order in a half-committed state where state is updated but orderPlacedAt is null. This indicates the design intent is atomicity across state mutation, saves, and hook-driven writes."###;
    assert_eq!(finding(title, statement, &spans), None, "finding-8");
    let spans = [evidence_1];
    let title = r###"Order lifecycle allows cancellation from every active state except Cancelled itself"###;
    let statement = r###"The transition table shows only the ArrangingPayment state can begin payment authorization or settlement, and that delivery and shipping states branch from PaymentSettled. Every pre-cancellation state lists Cancelled as a permitted target, indicating cancellation is a universal exit available throughout the pre-cancelled lifecycle, while Cancelled is terminal in this table."###;
    assert!(matches!(
        finding(title, statement, &spans),
        Some((_, _, Reason::Enumeration))
    ));
}
