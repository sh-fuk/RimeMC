package org.bukkit.event;

public abstract class Event {
    private final boolean async;
    public Event() { this(false); }
    public Event(boolean isAsync) { async = isAsync; }
    public String getEventName() { return getClass().getSimpleName(); }
    public abstract HandlerList getHandlers();
    public final boolean isAsynchronous() { return async; }
    public enum Result { DENY, DEFAULT, ALLOW }
}
