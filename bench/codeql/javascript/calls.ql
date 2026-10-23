/** Every call CodeQL resolves (precisely) to a function defined in the project. */

import javascript

from DataFlow::InvokeNode call, Function f
where
  f = call.getACallee(0) and
  exists(call.getFile().getRelativePath()) and
  exists(f.getFile().getRelativePath())
select call.getFile().getRelativePath(), call.getStartLine(), f.getName(), f.getFile().getRelativePath(),
  f.getLocation().getStartLine()
