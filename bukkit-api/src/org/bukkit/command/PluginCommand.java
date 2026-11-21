package org.bukkit.command;

import java.util.ArrayList;
import java.util.List;
import java.util.Map;

import org.bukkit.plugin.Plugin;

public final class PluginCommand extends Command {
    private final Plugin owner;
    private CommandExecutor executor;
    private TabCompleter completer;

    @SuppressWarnings("unchecked")
    public PluginCommand(String name, Plugin owner, Map<String, Object> spec) {
        super(name);
        this.owner = owner;
        this.executor = owner;
        if (spec != null) {
            if (spec.get("description") != null) setDescription(String.valueOf(spec.get("description")));
            if (spec.get("usage") != null) setUsage(String.valueOf(spec.get("usage")));
            if (spec.get("permission") != null) setPermission(String.valueOf(spec.get("permission")));
            if (spec.get("permission-message") != null) setPermissionMessage(String.valueOf(spec.get("permission-message")));
            Object a = spec.get("aliases");
            List<String> al = new ArrayList<>();
            if (a instanceof List) for (Object o : (List<Object>) a) al.add(String.valueOf(o));
            else if (a != null) al.add(String.valueOf(a));
            setAliases(al);
        }
    }

    @Override public boolean execute(CommandSender sender, String label, String[] args) {
        if (!owner.isEnabled()) return false;
        if (!testPermission(sender)) return true;
        boolean ok;
        try { ok = executor.onCommand(sender, this, label, args); }
        catch (Throwable t) {
            dev.rime.bukkit.Bridge.log(2, "Unhandled exception executing command '" + label + "' in plugin " + owner.getName() + ": " + t);
            sender.sendMessage("§cAn internal error occurred while attempting to perform this command");
            return true;
        }
        if (!ok && getUsage() != null && !getUsage().isEmpty())
            for (String line : getUsage().replace("<command>", label).split("\n")) sender.sendMessage(line);
        return ok;
    }

    public void setExecutor(CommandExecutor e) { executor = e == null ? owner : e; }
    public CommandExecutor getExecutor() { return executor; }
    public void setTabCompleter(TabCompleter c) { completer = c; }
    public TabCompleter getTabCompleter() { return completer; }
    public Plugin getPlugin() { return owner; }
}
