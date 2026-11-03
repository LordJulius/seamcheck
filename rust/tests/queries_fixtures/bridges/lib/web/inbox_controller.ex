defmodule Web.InboxController do
  def index(conn), do: Inbox.list(conn)
end
