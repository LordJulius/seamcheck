/** Every call CodeQL resolves to a function or method defined in the project. */

import go

from DataFlow::CallNode call, FuncDef f
where
  f = call.getACallee() and
  exists(call.getFile().getRelativePath()) and
  exists(f.getFile().getRelativePath())
select call.getFile().getRelativePath(), call.getStartLine(), f.getName(), f.getFile().getRelativePath(),
  f.getLocation().getStartLine()
