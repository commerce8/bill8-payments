# Bill8:Payments - Business Requirements & Functionality

> Audience: product owners, business analysts, and anyone evaluating what
> Bill8:Payments **does for the business**. For architecture read
> `README.md`; for wire-level detail read `DESIGN.md`.

## 1. Business Purpose

Bill8:Payments is the **system of record for payment acceptance**. It
delivers:

- **Frictionless acceptance** - merchants accept one-off payments with a
  customer and payment-method abstraction, no card-handling code.
- **Rail choice** - every merchant selects the settlement channels that
  fit their business: the platform's own finance rails, external
  acquirers/payment platforms, or direct bank connections. Channels are
  selectable and combinable per merchant with declarative routing by
  cost, payment method, and geography - and new channels onboard
  without re-engineering the platform.
- **Provable charge integrity** - every charge is recorded immutably;
  refunds and disputes are compensating records, so the audit trail is
  always replayable.
- **Safe retries** - idempotent APIs make a retried request harmless, so
  a dropped response never double-charges a customer.
- **Marketplace splits** - a single charge can split to a connected
  seller with an explicit platform fee, in one atomic operation.

## 2. Business Scope (What the Service Provides)

| # | Capability | What the business gets |
|---|---|---|
| 1 | Customers | Customer registry linked to platform identity |
| 2 | Payment methods | Saved instruments (secret-by-reference) |
| 3 | Payment intents | Intent-to-pay with amount/currency/status |
| 4 | Charges | Immutable money captures with a lifecycle |
| 5 | Refunds | Bounded compensating records |
| 6 | Disputes | Challenges with evidence and clawback |
| 7 | Setup intents | Save a method for later use |
| 8 | Tokens | Single-use references for card data (client-side collectable) |
| 9 | Settlement channels | Selectable rails - platform finance, external acquirers/platforms, bank rails - with per-merchant selection, declarative routing, and reconciliation |
| 10 | Test mode | Deterministic simulated instruments so integrators build against safe, predictable outcomes |

**What the service does NOT own (business boundary):**

| Out of scope | Owned by |
|---|---|
| Money movement / balances | Bill8:Ledger |
| Recurring billing | Bill8:Billing |
| Hosted checkout UI | Bill8:Checkout |
| Connected accounts | Bill8:Merchants |
| Fraud rules | Bill8:Risk |
| Event delivery | Bill8:Webhooks |

## 3. Business Capabilities

| Capability | Business rule |
|---|---|
| Charge a customer | A charge routes through a selected channel and moves money through the ledger; it is recorded immutably |
| Choose rails | A merchant selects a default channel and can route by cost, method, and geography; channels onboard without platform changes |
| Refund a charge | A refund reverses a charge, bounded by the remaining refundable amount |
| Dispute a charge | A dispute challenges a charge and can claw back funds |
| Retry safely | A retried request is idempotent - never double-moves money |
| Split a charge | A destination charge splits to a seller with an explicit platform fee, atomically |
| Build in test mode | Integrators use deterministic test instruments; test traffic can never reach live rails |

## 4. Business Events (Outbound)

| Event | Fires when |
|---|---|
| `customer:created/updated/deleted` | Customer lifecycle changes |
| `payment_method:created/updated` | Payment method changes |
| `intent:created/updated` | Intent lifecycle changes |
| `charge:succeeded/failed` | A charge reaches a terminal state |
| `refund:created/updated` | A refund is created/updated |
| `dispute:created/updated/closed` | A dispute lifecycle changes |

## 5. Business Rules & Invariants

1. Charges are append-only - never mutated.
2. A refund can never exceed the remaining refundable amount.
3. Money is only moved through Bill8:Ledger.
4. Terminal states never regress.
5. Every lifecycle change emits an event.

## 6. Target-User Roles & Use Cases

| Persona | Does |
|---|---|
| Merchant | Accept payments, issue refunds, view charges |
| Customer | Pay via hosted checkout |
| Platform operator | Oversee charge/refund activity |

## 7. KPIs / Success Criteria

| KPI | Measure |
|---|---|
| Charge success rate | succeeded / attempted |
| Refund integrity | zero over-refunds, zero double-refunds |
| Idempotency | retried requests never double-move money |

## 8. Dependencies & Reliance on Other Services

| Reliance | On |
|---|---|
| Money movement | Bill8:Ledger |
| Fraud screening | Bill8:Risk |
| Identity | Sentin8:Auth |
| Event transport | Sentin8:Bus |

## 9. Acceptance Criteria

1. A merchant can charge a customer end-to-end with money recorded in
   the ledger.
2. A refund fully reverses a charge and cannot be double-executed.
3. A retried request is idempotent.

## 10. Open Business Questions

- Card-network certification is external by design (see `backlog.md`).

---

**Design attribution:** bill8:payments and its underlying concepts were designed by Hamid Jahanzad <Hamid@Jahanzad.com>.
