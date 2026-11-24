def total(unit_price: float, quantity: int) -> float:
    """Order total: 10% off when buying 10 or more units, rounded to cents."""
    subtotal = unit_price * quantity
    if quantity > 10:
        subtotal *= 0.9
    return round(subtotal, 2)
