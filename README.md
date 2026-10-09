# Backend Assignment

## Loom videos

First Video(Data Model and Testing the endpoint):
https://www.loom.com/share/b3e636c63ded4793bf5f42ac76a91b40

Second Video(State Machine walkthrough and Webhook testing and testing all the code token when paying)

Third Video(testing tok_timeout)
https://www.loom.com/share/35772b4ae63e4eecbc68543c437c5042

## Instructions

To run the app, run the following command

```
docker compose up
```

This starts the postgres instance, the invoice app and the mock psp.

## Curl Examples

When the app runs, it automatically creates a business with an API key `sk_demo0001_devonlysecretdevonlysecret00`

So add this as the bearer token to every request

### Create a customer

Run the following curl command to create a customer

```
curl --location 'http://localhost:8000/customers' \
--header 'Content-Type: application/json' \
--header 'Authorization: Bearer sk_demo0001_devonlysecretdevonlysecret00' \
--data-raw '{
    "name":"Ada Lovelace",
    "email":"ada@example.com"
}'
```

Response

```
{"
    id":"7cba841f-8498-46a4-81ff-b8b36ad09ad5",
    "name":"Ada Lovelace",
    "email":"ada@example.com",
    "created_at":"2026-10-09T06:01:55.556634Z"
}
```

### Get all Customers

```
curl --location --request GET 'http://localhost:8000/customers' \
--header 'Content-Type: application/json' \
--header 'Authorization: Bearer sk_demo0001_devonlysecretdevonlysecret00' \
--data-raw '{
    "name":"Ada Lovelace",
    "email":"ada@example.com"
}'
```

Response:

```
{
    "data": [
        {
            "id": "7cba841f-8498-46a4-81ff-b8b36ad09ad5",
            "name": "Ada Lovelace",
            "email": "ada@example.com",
            "created_at": "2026-10-09T06:01:55.556634Z"
        }
    ]
}
```


### Create an Invoice

```
curl --location 'http://localhost:8000/invoices' \
--header 'Idempotency-Key: pay-2' \
--header 'Content-Type: application/json' \
--header 'Authorization: Bearer sk_demo0001_devonlysecretdevonlysecret00' \
--data '{
    "customer_id":"44007948-d005-44a0-8323-ff68b0160a1f",
    "due_date":"2026-11-01",
    "line_items":[{"description":"Widget","quantity":3,"unit_amount_cents":1999}]
}'
```

Response:

```
{
    "id":"6c74c953-477f-4167-afbd-a2863ac3847b","customer_id":"7cba841f-8498-46a4-81ff-b8b36ad09ad5",
    "state":"draft",
    "total_cents":5997,
    "due_date":"2026-11-01",
    "created_at":"2026-10-09T06:07:04.556177Z","updated_at":"2026-10-09T06:07:04.556177Z",
    "paid_at":null,
    "line_items":[
        {"description":"Widget",
        "quantity":3,
        "unit_amount_cents":1999,
        "amount_cents":5997
        }
    ]
}
```

### Get all Invoices

```
curl --location --request GET 'http://localhost:8000/invoices' \
--header 'Idempotency-Key: pay-2' \
--header 'Content-Type: application/json' \
--header 'Authorization: Bearer sk_demo0001_devonlysecretdevonlysecret00' \
--data '{
    "customer_id":"7cba841f-8498-46a4-81ff-b8b36ad09ad5",
    "due_date":"2026-11-01",
    "line_items":[{"description":"Widget","quantity":3,"unit_amount_cents":1999}]
}'
```

Response:

```
{
    "data":[
        {
            "id":"6c74c953-477f-4167-afbd-a2863ac3847b","customer_id":"7cba841f-8498-46a4-81ff-b8b36ad09ad5","state":"draft",
            "total_cents":5997,
            "due_date":"2026-11-01",
            "created_at":"2026-10-09T06:07:04.556177Z","updated_at":"2026-10-09T06:07:04.556177Z",
            "paid_at":null
        }
    ]
}
```

### Finalize an Invoice

```
curl --location 'http://localhost:8000/invoices/6c74c953-477f-4167-afbd-a2863ac3847b/finalize' \
--header 'Content-Type: application/json' \
--header 'Authorization: Bearer sk_demo0001_devonlysecretdevonlysecret00' \
--data '{
    "customer_id":"44007948-d005-44a0-8323-ff68b0160a1f",
    "due_date":"2026-11-01",
    "line_items":[{"description":"Widget","quantity":3,"unit_amount_cents":1999}]
}'
```

Response:

```
{
    "id":"6c74c953-477f-4167-afbd-a2863ac3847b","customer_id":"7cba841f-8498-46a4-81ff-b8b36ad09ad5",
    "state":"open",
    "total_cents":5997,
    "due_date":"2026-11-01",
    "created_at":"2026-10-09T06:07:04.556177Z","updated_at":"2026-10-09T06:11:22.602415Z",
    "paid_at":null,
    "line_items":[
        {
            "description":"Widget",
            "quantity":3,
            "unit_amount_cents":1999,
            "amount_cents":5997
        }
    ]
}
```

### Register a Endpoint for Webhook

```
curl --location 'http://localhost:8000/webhook_endpoints' \
--header 'Content-Type: application/json' \
--header 'Authorization: Bearer sk_demo0001_devonlysecretdevonlysecret00' \
--data '{"url":"http://mock-psp:9000/webhooks/sink"}'
```

Response:

```
{
    "id": "5f2da0f6-1e30-4ae9-bb1b-ea82e85cb590",
    "url": "http://mock-psp:9000/webhooks/sink",
    "secret": "whsec_381a133bb67d4aa7b663decd863ff66ce3a841772db94b369d61fc72eb8d4e67",
    "is_active": true,
    "created_at": "2026-10-09T06:21:20.643576Z"
}
```


### Paying the Invoice

There are multiple card token:
    tok_success            -> succeeded after ~100 ms
    tok_insufficient_funds -> failed (insufficient_funds) after ~100 ms
    tok_card_declined      -> failed (card_declined) after ~100 ms
    tok_timeout            -> succeeded, but only after 30 s
    tok_network_error      -> HTTP 500 immediately, nothing is charged

```
curl --location 'http://localhost:8000/invoices/6c74c953-477f-4167-afbd-a2863ac3847b/pay' \
--header 'Idempotency-Key: pay-25' \
--header 'Content-Type: application/json' \
--header 'Authorization: Bearer sk_demo0001_devonlysecretdevonlysecret00' \
--data '{"card_token": "tok_success" }'
```

Response:

```
{
    "id":"a5f58164-c41d-484d-be92-222738074469","invoice_id":"6c74c953-477f-4167-afbd-a2863ac3847b","status":"succeeded",
    "amount_cents":5997,
    "psp_ref":"f47f0457-dce9-49a1-b601-fe4a59b5b258",
    "failure_code":null,
    "invoice_state":"paid"
}
```