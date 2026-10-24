fun from(t: String) = when (t.removeSuffix("_embed")) {
    "weather" -> 1
    "quiz" -> 2
    else -> 0
}
