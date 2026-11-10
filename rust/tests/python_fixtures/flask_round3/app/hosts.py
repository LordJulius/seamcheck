from pathlib import Path


class HostController:
    roots = [Path("/srv/data/exports"), Path("/srv/data/uploads")]

    def _is_allowed_path(self, path):
        resolved = Path(path).resolve()
        for root in self.roots:
            try:
                resolved.relative_to(root)
                return resolved
            except ValueError:
                continue
        raise PermissionError(path)

    def resolve_managed_path(self, raw, must_exist=False):
        path = self._is_allowed_path(raw)
        if must_exist and not path.exists():
            raise FileNotFoundError(raw)
        return path


host_controller = HostController()
