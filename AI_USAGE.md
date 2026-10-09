## Tools used

Claude to draft the migrations and the router handler functions and auth

## Decisions I made myself

- The State Transition

What the AI proposed: once a invoice is created, directly make it open and be able to pay from the inital state

What I chose: The first state of an invoice will be "draft", upon which it will be finalized and its state will be "open" and from there customer can pay and the state will be changed to "paid"

Reasoning: Starting off an invoice as "open" is archtecturally wrong and isn't good for the business as a whole

- How to treat an ambiguous PSP result(tok_network_error, timeouts)

What the AI proposed: leave the attempt pending, give up to unknown after 120 s, and keep the invoice locked. When I hit this, it offered three options: keep that as is, treat a PSP 5xx as a plain failure, or keep re-checking unknown attempts.

What I chose: the sweeper keeps re-asking the PSP about unknown attempts (about once a minute) using the same idempotency key, and unknown still blocks new attempts.

Reasoning: This would be the correct archetectural design as leaving a dangling invoice isn't good for the buisiness

- How the API keys and Business are built

What the AI proposed: Have the invoice system create the api keys and business directly.

What i chose: Hard coded a business to be created when running the migrations. Same with the API keys which are scoped to a business.

Reasoning: Having the invoice api create the business and api keys isn't right

## One thing the AI got wrong

Unresolved payment attempts were never re-checked, so an invoice could stay locked forever. In its first version of the /pay flow, the sweeper marked an attempt unknown after 120 s and then stopped trying. I tested tok_network_error twice on the same invoice and got 409 payment_in_progress the second time. The AI first explained that the lock was intentional, then acknowledged the real gap: even if the PSP recovered later, nobody would ask it, so the invoice stayed blocked until someone edited the database by hand. Fix: finalize now accepts attempts in unknown, and the sweeper re-checks them. Safe because the PSP dedups on the attempt id.