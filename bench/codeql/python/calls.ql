/**
 * Every call CodeQL resolves to a function defined in the project:
 * (call file, call line, callee name, callee file, callee line).
 */

import python
import semmle.python.dataflow.new.internal.DataFlowDispatch as DD

from DD::DataFlowCall call, DD::DataFlowCallable c, Function target
where
  c = DD::viableCallable(call) and
  target = c.getScope() and
  exists(call.getLocation().getFile().getRelativePath()) and
  exists(target.getLocation().getFile().getRelativePath())
select call.getLocation().getFile().getRelativePath(), call.getLocation().getStartLine(),
  target.getName(), target.getLocation().getFile().getRelativePath(),
  target.getLocation().getStartLine()
