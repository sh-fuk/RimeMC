package dev.rime.bukkit;

import java.util.logging.Logger;

public final class RimeLoggers {
    private RimeLoggers() {}
    public static Logger forPlugin(String name) { return RimeLogger.create(name); }
}
