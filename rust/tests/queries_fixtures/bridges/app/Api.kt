class Api {
    fun inbox(id: String) {
        get("$root/api/inbox/$id")
    }
}
