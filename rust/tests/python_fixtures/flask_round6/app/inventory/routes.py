from flask import Blueprint, request, abort, send_file
from flask_login import login_required, current_user
from app.models import Division
from app.utils.audit import log_download
from app.utils.time import utc_display, utc_now
bp = Blueprint('inventory', __name__)
def _resolve_accessible_division_id(raw_division_id):
    if not raw_division_id:
        return None
    try:
        div_id = int(raw_division_id)
    except ValueError:
        return None
    user_divisions = get_user_divisions()
    if any(division.id == div_id for division in user_divisions) or current_user.has_effective_admin():
        return div_id
    return None
def _get_low_stock_items(division_id):
    return _load_inventory_items_by_ids(_load_low_stock_item_ids(division_id))
@bp.route('/low-stock/export/pdf')
@login_required
def export_low_stock_pdf():
    if not current_user.has_permission('inventory', 'export'):
        abort(403)
    from app.utils.pdf_generator import render_pdf, format_currency
    division_id = request.args.get('division_id', '').strip()
    resolved_division_id = _resolve_accessible_division_id(division_id)
    if resolved_division_id is not None:
        division = Division.query.get(resolved_division_id)
        division_name = division.name if division else 'Selected Division'
    else:
        division_name = 'All Divisions'
    items = _get_low_stock_items(resolved_division_id)
    pdf_buffer = render_pdf('inventory_low_stock.html', items=[])
    log_download('Inventory', None, f'Exported. {len(items)} items.')
    filename = f'low_stock_report_{utc_now().strftime("%Y%m%d_%H%M%S")}.pdf'
    return send_file(pdf_buffer, mimetype='application/pdf', as_attachment=True, download_name=filename)


@bp.route('/low-stock/export/report')
def export_low_stock_report():
    from app.utils.reports import render_summary
    items = _get_low_stock_items(request.args.get('division_id'))
    report = render_summary('inventory_low_stock.html', items=[])
    return f'{report} {len(items)}'


@bp.route('/low-stock/export/logo')
def export_logo():
    from app.utils.reports import render_report
    return render_report('inventory_low_stock.html', logo_path=request.args['logo'])  # TP: the keyword's value reaches open()
