# Alfred — Business & Merchant Information

This page describes Alfred's business identity, product, support and customer policies. It exists to satisfy the KYC ("Know Your Customer") requirements of payment processors and merchants of record (Lemon Squeezy, Stripe, Paddle…).

If anything here is unclear, please contact us — see *Customer Support* below.

## Business Identity

- **Operator** : Pierre Derrier — individual operator (auto-entrepreneur, France).
- **Country of operation** : France (European Union).
- **Public contact** : see *Customer Support* below.

> Legal entity registration number and VAT will be added here once finalized.

## Product

**Alfred** is a personal portfolio analysis copilot for retail investors who manage their own stock investments. It connects to the user's portfolio aggregator (Finary), enriches each line with public market data, news and technical indicators, and surfaces a weekly summary plus actionable suggestions in a desktop application.

- **Distribution** : open-source desktop binary for Windows (MSI) and macOS (DMG). Source code: <https://github.com/pderrier/alfred-desktop>.
- **What runs where** : the desktop app runs entirely on the user's machine. A small backend service (`alfred-api`) on a dedicated VPS proxies third-party data collection. No user portfolio data is persisted server-side beyond ephemeral cache.
- **Authentication** : the user signs in with their own OpenAI account (OAuth). Alfred derives a one-way hash of the OpenAI account token for rate-limit accounting; the token itself never reaches Alfred's servers.

## Pricing

- **Free tier** : 3 portfolio analyses per rolling 7-day window. Permanent, no time limit, no credit card required.
- **Premium** : **€9 per year**, unlimited analyses. Billed annually, cancellable anytime.

Premium payments are processed by **Lemon Squeezy** (Merchant of Record for EU VAT). Alfred does not handle card details directly.

## Customer Support

- **Email** : `support@alfred-app.io` *(to be confirmed once domain is provisioned — temporarily use GitHub Issues below)*
- **GitHub Issues** : <https://github.com/pderrier/alfred-desktop/issues>
- **Response window** : best-effort, typically within 3 business days.

For premium subscription matters (billing, refunds, cancellations), customers may also contact Lemon Squeezy's customer portal directly via the link in their activation email.

## Refund Policy

Alfred Premium subscribers benefit from the **EU 14-day right of withdrawal** for digital services (Directive 2011/83/EU).

- A full refund may be requested within 14 days of the purchase date.
- After 14 days, the subscription is non-refundable for the remainder of the billing period; cancellation prevents further charges (see below).
- Refund requests : contact our support email or use Lemon Squeezy's customer portal.

## Cancellation Policy

- Premium can be cancelled at any time via the Lemon Squeezy customer portal (link sent in the activation email).
- Cancellation takes effect at the end of the current billing period; no further charges occur.
- The premium features remain active until the end of the paid period.

## Restrictions

Alfred is a **software tool**. It is **not** financial advice and **not** a regulated investment service. All investment decisions are the user's sole responsibility.

- Alfred Premium is available globally, restricted only by Lemon Squeezy's supported regions (see <https://www.lemonsqueezy.com> for the authoritative list).
- The free tier is available to anyone able to install the desktop application and authenticate with an OpenAI account.

## Terms summary

By using Alfred, the user agrees to:

1. Be **18 years or older** and legally able to enter binding contracts.
2. Use Alfred only as a **research and discussion aid**, not as a source of financial advice.
3. Maintain the confidentiality of their OpenAI account credentials — Alfred never stores them but they unlock access to Alfred's API quota.
4. Source code license : permissive open-source license (see `LICENSE` in this repository).

A full *Terms of Service* and *Privacy Policy* will be linked here once published.

---

*Last updated: 2026-05-16.*
