defmodule A do
  def ok do
    x()
  rescue
    e -> Logger.warning(e)
  end

  def bad do
    x()
  rescue
    _ -> :error
  end

  def p do
    push(1)
    ack()
    y()
    push(2)
  end
end
