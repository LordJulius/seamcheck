/** Every call CodeQL resolves to a method defined in the project. */

import codeql.ruby.AST
import codeql.ruby.dataflow.internal.DataFlowDispatch as DD

from DD::DataFlowCall call, MethodBase m
where
  m = DD::viableCallable(call).asCfgScope() and
  exists(call.getLocation().getFile().getRelativePath()) and
  exists(m.getLocation().getFile().getRelativePath())
select call.getLocation().getFile().getRelativePath(), call.getLocation().getStartLine(), m.getName(),
  m.getLocation().getFile().getRelativePath(), m.getLocation().getStartLine()
