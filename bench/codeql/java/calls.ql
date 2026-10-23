/** Every call CodeQL resolves (with virtual dispatch) to a method or constructor in the project. */

import java
import semmle.code.java.dispatch.VirtualDispatch

from Call c, Callable t
where
  t = viableImpl(c) and
  t.fromSource() and
  exists(c.getFile().getRelativePath()) and
  exists(t.getFile().getRelativePath())
select c.getFile().getRelativePath(), c.getLocation().getStartLine(), t.getName(), t.getFile().getRelativePath(),
  t.getLocation().getStartLine()
