package org.bukkit;

public enum GameMode {
    CREATIVE(1), SURVIVAL(0), ADVENTURE(2), SPECTATOR(3);
    private final int value;
    GameMode(int v) { value = v; }
    @Deprecated public int getValue() { return value; }
    @Deprecated public static GameMode getByValue(int v) { for (GameMode g : values()) if (g.value == v) return g; return null; }
}
