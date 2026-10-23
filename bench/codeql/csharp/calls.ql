/** Every call CodeQL resolves (runtime targets) to a callable defined in the project. */

import csharp

from Call c, Callable t
where
  t = c.getARuntimeTarget() and
  t.fromSource() and
  exists(c.getFile().getRelativePath()) and
  exists(t.getFile().getRelativePath())
select c.getFile().getRelativePath(), c.getLocation().getStartLine(), t.getName(), t.getFile().getRelativePath(),
  t.getLocation().getStartLine()
