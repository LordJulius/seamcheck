from contextlib import contextmanager
from typing import Any, Optional

import pymssql


@contextmanager
def b2w_connection(preview: bool = False):
    conn = pymssql.connect(server="x")
    try:
        yield conn
    finally:
        conn.close()


def b2w_fetchall(sql: str, params: Optional[tuple] = None, *, conn: Any = None, preview: bool = False):
    if conn is not None:
        cur = conn.cursor(as_dict=True)
        cur.execute(sql, params or ())
        rows = cur.fetchall()
        return rows
    with b2w_connection(preview=preview) as owned:
        return b2w_fetchall(sql, params, conn=owned)


def _header_sql_select() -> str:
    return """h.TicketNumber, h.Customer, h.Total"""


def _find_ticket_headers(ticket_number, *, conn, preview=False):
    candidates = [ticket_number, ticket_number.strip(), ticket_number.upper()]
    exact = []
    for c in candidates:
        if c and c not in exact:
            exact.append(c)
    for c in list(exact):
        if c.isdigit():
            exact.append(c.zfill(8))
    placeholders = ",".join(["%s"] * len(exact))
    header_from = f"""FROM TicketHeader h
        LEFT JOIN Customer c ON c.Id = h.CustomerId"""
    order_sql = """ORDER BY h.TicketDate DESC"""
    return b2w_fetchall(
        f"""SELECT {_header_sql_select()} {header_from}
        WHERE h.TicketNumber IN ({placeholders}) OR h.AltNumber IN ({placeholders}) {order_sql}""",
        tuple(exact) + tuple(exact),
        conn=conn,
    )


def fetch_b2w_ticket_bundle(ticket_number, *, preview=False):
    with b2w_connection(preview=preview) as conn:
        return {"headers": _find_ticket_headers(ticket_number, conn=conn, preview=preview)}


def lookup_b2w_ticket(ticket_number, preview=False):
    return fetch_b2w_ticket_bundle(ticket_number, preview=preview)
