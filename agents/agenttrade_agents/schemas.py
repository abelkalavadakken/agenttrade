"""Output schemas. The trader's schema mirrors SubmitIntentRequest in
proto/agenttrade/v1. Prices and quantities are decimal strings; the Rust
side parses them to fixed-point at the instrument's scale. No floats."""

from __future__ import annotations

DECIMAL = {"type": "string", "pattern": r"^-?[0-9]+(\.[0-9]+)?$"}

OBSERVER = {
    "type": "object",
    "additionalProperties": False,
    "required": ["narrative"],
    "properties": {"narrative": {"type": "string"}},
}

ANALYST = {
    "type": "object",
    "additionalProperties": False,
    "required": ["read", "regime", "bias", "confidence"],
    "properties": {
        "read": {"type": "string"},
        "regime": {"type": "string", "enum": ["trend_up", "trend_down", "range", "unclear"]},
        "bias": {"type": "string", "enum": ["long", "short", "flat"]},
        "confidence": {"type": "number", "minimum": 0, "maximum": 1},
    },
}

INTENT = {
    "type": "object",
    "additionalProperties": False,
    "required": ["intent_type", "side", "time_in_force", "target_price", "stop_loss", "quantity", "reason"],
    "properties": {
        "intent_type": {"type": "string", "enum": ["PLACE", "CANCEL", "FLATTEN", "NOOP"]},
        "side": {"type": "string", "enum": ["BUY", "SELL", "SIDE_UNSPECIFIED"]},
        "time_in_force": {"type": "string", "enum": ["GTC", "IOC", "FOK", "TIF_UNSPECIFIED"]},
        "target_price": DECIMAL,
        "stop_loss": DECIMAL,
        "quantity": DECIMAL,
        "reason": {"type": "string"},
    },
}

TRADER = {
    "type": "object",
    "additionalProperties": False,
    "required": ["intents"],
    "properties": {"intents": {"type": "array", "maxItems": 3, "items": INTENT}},
}

REVIEWER = {
    "type": "object",
    "additionalProperties": False,
    "required": ["verdicts"],
    "properties": {
        "verdicts": {
            "type": "array",
            "items": {
                "type": "object",
                "additionalProperties": False,
                "required": ["index", "pass", "reason"],
                "properties": {
                    "index": {"type": "integer", "minimum": 0},
                    "pass": {"type": "boolean"},
                    "reason": {"type": "string"},
                },
            },
        }
    },
}
