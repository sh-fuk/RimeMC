package org.bukkit.command;

import java.util.ArrayList;
import java.util.List;

public abstract class Command {
    private final String name;
    protected String description = "";
    protected String usageMessage;
    private String permission;
    private String permissionMessage;
    private List<String> aliases = new ArrayList<>();

    protected Command(String name) { this.name = name; this.usageMessage = "/" + name; }
    public abstract boolean execute(CommandSender sender, String commandLabel, String[] args);
    public String getName() { return name; }
    public String getDescription() { return description; }
    public Command setDescription(String d) { description = d == null ? "" : d; return this; }
    public String getUsage() { return usageMessage; }
    public Command setUsage(String u) { usageMessage = u == null ? "" : u; return this; }
    public List<String> getAliases() { return aliases; }
    public Command setAliases(List<String> a) { aliases = new ArrayList<>(a); return this; }
    public String getPermission() { return permission; }
    public void setPermission(String p) { permission = p; }
    public String getPermissionMessage() { return permissionMessage; }
    public Command setPermissionMessage(String m) { permissionMessage = m; return this; }
    public boolean testPermission(CommandSender target) {
        if (permission == null || permission.isEmpty() || target.hasPermission(permission)) return true;
        target.sendMessage(permissionMessage != null ? permissionMessage : "§cI'm sorry, but you do not have permission to perform this command.");
        return false;
    }
    @Override public String toString() { return getClass().getName() + "(" + name + ")"; }
}
