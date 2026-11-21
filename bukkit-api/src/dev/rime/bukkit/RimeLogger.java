package dev.rime.bukkit;

import java.util.logging.Handler;
import java.util.logging.Level;
import java.util.logging.LogRecord;
import java.util.logging.Logger;

final class RimeLogger {
    static Logger create(String prefix) {
        Logger l = Logger.getLogger("rime." + (prefix.isEmpty() ? "server" : prefix));
        l.setUseParentHandlers(false);
        for (Handler h : l.getHandlers()) l.removeHandler(h);
        String pre = prefix.isEmpty() ? "" : "[" + prefix + "] ";
        l.addHandler(new Handler() {
            @Override public void publish(LogRecord r) {
                String msg = r.getMessage();
                if (r.getParameters() != null) { try { msg = java.text.MessageFormat.format(msg, r.getParameters()); } catch (Exception ignored) {} }
                if (r.getThrown() != null) msg += " " + r.getThrown();
                int lvl = r.getLevel().intValue() >= Level.SEVERE.intValue() ? 2 : r.getLevel().intValue() >= Level.WARNING.intValue() ? 1 : 0;
                Bridge.log(lvl, pre + msg);
            }
            @Override public void flush() {}
            @Override public void close() {}
        });
        return l;
    }
}
